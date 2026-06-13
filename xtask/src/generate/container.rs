//! Shared renderer for container/build surfaces (Containerfile, Dockerfile,
//! Dockerfile.debian) that inject a cargo `--features` line per build stage.
//! Each injection point is a sentinel-delimited zone naming the `Selection` it
//! renders; the feature list is resolved from the canonical spec, never typed.
//!
//! Hash-comment syntax (`#`) for Dockerfile/Containerfile.

use super::spec::{self, Selection};
use std::path::Path;

fn begin(zone: &str) -> String {
    format!("# >>> generated:{zone} by `cargo generate installers` — do not edit <<<")
}
fn end(zone: &str) -> String {
    format!("# >>> end generated:{zone} <<<")
}

/// Render the feature-arg body for a zone: a `--features "X,Y" \` continued
/// line a `cargo build` consumes (the surrounding `--no-default-features \`
/// stays in the hand-written template). For the default selection, emits the
/// `--features` line resolved from Cargo's default leaves so the image is
/// explicit and drift-checkable rather than relying on implicit defaults.
pub fn render_features(
    manifest_dir: &Path,
    selection: &Selection,
    indent: &str,
) -> anyhow::Result<String> {
    let list = spec::resolve_feature_list(manifest_dir, selection)?;
    Ok(format!("{indent}--features \"{}\" \\", list.join(",")))
}

/// Render an `ARG ZEROCLAW_CARGO_FEATURES="X,Y"` default line from a selection.
/// The ARG stays build-time overridable (the docker feature-selection axis);
/// only its default is canonical. The list is resolved, never typed.
pub fn render_features_arg(manifest_dir: &Path, selection: &Selection) -> anyhow::Result<String> {
    let list = spec::resolve_feature_list(manifest_dir, selection)?;
    Ok(format!(
        "ARG ZEROCLAW_CARGO_FEATURES=\"{}\"",
        list.join(",")
    ))
}

/// Splice a named zone's body into `current`, preserving everything else.
pub fn splice(current: &str, zone: &str, body: &str) -> anyhow::Result<String> {
    let b = begin(zone);
    let e = end(zone);
    let begin_at = current
        .find(&b)
        .ok_or_else(|| anyhow::anyhow!("missing generated:{zone} BEGIN sentinel"))?;
    let after_begin = begin_at + b.len();
    let end_rel = current[after_begin..]
        .find(&e)
        .ok_or_else(|| anyhow::anyhow!("missing generated:{zone} END sentinel"))?;
    let end_at = after_begin + end_rel;
    let mut out = String::new();
    out.push_str(&current[..after_begin]);
    out.push('\n');
    out.push_str(body);
    out.push('\n');
    out.push_str(&current[end_at..]);
    Ok(out)
}

/// A container surface = a file plus its named feature zones, each bound to a
/// selection. Drives both render and check generically.
pub struct ContainerSurface {
    pub file: &'static str,
    /// (zone name, selection, indent) per injection point.
    pub zones: Vec<(&'static str, Selection, &'static str)>,
}

impl ContainerSurface {
    pub fn render(&self, manifest_dir: &Path, current: &str) -> anyhow::Result<String> {
        let mut out = current.to_string();
        for (zone, sel, indent) in &self.zones {
            let body = render_features(manifest_dir, sel, indent)?;
            out = splice(&out, zone, &body)?;
        }
        Ok(out)
    }
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
    fn full_renders_explicit_default_leaves() {
        let b = render_features(&root(), &Selection::Full, "        ").unwrap();
        // Full now emits the explicit resolved default leaves (drift-checkable),
        // not a bare comment.
        assert!(b.contains("--features"));
        assert!(b.contains("gateway"), "default includes gateway");
    }

    #[test]
    fn dist_renders_all_channels() {
        let b = render_features(&root(), &Selection::Dist, "        ").unwrap();
        assert!(b.contains("channel-discord") && !b.contains("hardware"));
    }

    #[test]
    fn all_renders_kitchen_sink() {
        let b = render_features(&root(), &Selection::All, "        ").unwrap();
        assert!(b.contains("hardware") && b.contains("channel-matrix"));
    }

    #[test]
    fn splice_named_zone() {
        let cur = format!("X\n{}\nOLD\n{}\nY\n", begin("z1"), end("z1"));
        let out = splice(&cur, "z1", "NEW").unwrap();
        assert!(out.contains("NEW") && !out.contains("OLD"));
    }
}
