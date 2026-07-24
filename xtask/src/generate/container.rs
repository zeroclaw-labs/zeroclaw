//! Shared renderer for container/build surfaces (Containerfile, Dockerfile,
//! Dockerfile.debian) that inject a cargo `--features` line per build stage.
//! Each injection point is a sentinel-delimited zone naming the `Selection` it
//! renders; the feature list is resolved from the canonical spec, never typed.

use super::spec::{self, Selection};
use std::path::Path;

fn begin(zone: &str) -> String {
    format!("# >>> generated:{zone} by `cargo generate installers` - do not edit <<<")
}
fn end(zone: &str) -> String {
    format!("# >>> end generated:{zone} <<<")
}

pub fn render_features(
    manifest_dir: &Path,
    selection: &Selection,
    indent: &str,
) -> anyhow::Result<String> {
    let list = spec::resolve_feature_list(manifest_dir, selection)?;
    Ok(format!("{indent}ZEROCLAW_FEATURES=\"{}\"", list.join(",")))
}

pub fn render_features_arg(manifest_dir: &Path, selection: &Selection) -> anyhow::Result<String> {
    let flags = spec::resolve_flags(manifest_dir, selection)?;
    Ok(format!("ARG ZEROCLAW_CARGO_FLAGS=\"{flags}\""))
}

/// Splice a named zone's body into `current`, preserving everything else.
pub fn splice(current: &str, zone: &str, body: &str) -> anyhow::Result<String> {
    let b = begin(zone);
    let e = end(zone);
    let begin_at = current
        .find(&b)
        .ok_or_else(|| anyhow::Error::msg(format!("missing generated:{zone} BEGIN sentinel")))?;
    let after_begin = begin_at + b.len();
    let end_rel = current[after_begin..]
        .find(&e)
        .ok_or_else(|| anyhow::Error::msg(format!("missing generated:{zone} END sentinel")))?;
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
        let b = render_features(&root(), &Selection::Full, "    ").unwrap();
        // Full emits the explicit resolved default leaves as a ZEROCLAW_FEATURES
        // assignment (drift-checkable), not a bare comment.
        assert!(b.contains("ZEROCLAW_FEATURES="));
        assert!(b.contains("gateway"), "default includes gateway");
    }

    #[test]
    fn dist_renders_lean_release_channels() {
        let b = render_features(&root(), &Selection::Dist, "        ").unwrap();
        assert!(b.contains("channel-matrix"));
        assert!(b.contains("channel-lark"));
        assert!(b.contains("whatsapp-web"));
        assert!(!b.contains("channel-slack"));
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
