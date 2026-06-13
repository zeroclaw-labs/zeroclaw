//! Packaging surface renderers: AUR PKGBUILD (shell-comment sentinels) and the
//! scoop manifest (JSON, no comments — targeted key rewrite). Version and
//! feature sets come from the canonical spec; nothing is typed.

use super::spec::{self, Selection};
use std::path::Path;

fn begin(zone: &str) -> String {
    format!("# >>> generated:{zone} by `cargo generate installers` — do not edit <<<")
}
fn end(zone: &str) -> String {
    format!("# >>> end generated:{zone} <<<")
}

fn splice(current: &str, zone: &str, body: &str) -> anyhow::Result<String> {
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

/// PKGBUILD: regenerate the `pkgver` and the build `--features` from the spec.
/// Ships `Selection::Dist`. Two zones — version and the cargo build line.
pub fn render_pkgbuild(root: &Path, current: &str) -> anyhow::Result<String> {
    let version = spec::resolve_version(root)?;
    let features = spec::resolve_feature_list(root, &Selection::Dist)?.join(",");
    let with_ver = splice(current, "pkgbuild-version", &format!("pkgver={version}"))?;
    splice(
        &with_ver,
        "pkgbuild-build",
        &format!("  cargo build --frozen --profile dist --features {features}"),
    )
}

/// Scoop manifest is JSON (no comments). Rewrite the top-level `"version"`
/// value to the canonical workspace version via a targeted key replace,
/// preserving formatting everywhere else.
pub fn render_scoop(root: &Path, current: &str) -> anyhow::Result<String> {
    let version = spec::resolve_version(root)?;
    rewrite_json_version(current, &version)
}

fn rewrite_json_version(current: &str, version: &str) -> anyhow::Result<String> {
    // Match the first `"version": "..."` and replace its value. The manifest's
    // download URLs use the `$version` scoop variable, so only this one literal
    // needs updating.
    let key = "\"version\":";
    let key_at = current
        .find(key)
        .ok_or_else(|| anyhow::anyhow!("scoop manifest has no \"version\" key"))?;
    let after_key = key_at + key.len();
    let rest = &current[after_key..];
    let q1 = rest
        .find('"')
        .ok_or_else(|| anyhow::anyhow!("malformed version value"))?;
    let q2 = rest[q1 + 1..]
        .find('"')
        .ok_or_else(|| anyhow::anyhow!("unterminated version value"))?;
    let val_start = after_key + q1 + 1;
    let val_end = after_key + q1 + 1 + q2;
    let mut out = String::new();
    out.push_str(&current[..val_start]);
    out.push_str(version);
    out.push_str(&current[val_end..]);
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
    fn pkgbuild_version_matches_workspace() {
        let v = spec::resolve_version(&root()).unwrap();
        let cur = format!(
            "a\n{}\npkgver=0.0.0\n{}\nb\n",
            begin("pkgbuild-version"),
            end("pkgbuild-version")
        );
        let out = splice(&cur, "pkgbuild-version", &format!("pkgver={v}")).unwrap();
        assert!(out.contains(&format!("pkgver={v}")));
        assert!(!out.contains("pkgver=0.0.0"));
    }

    #[test]
    fn scoop_version_rewritten() {
        let out =
            rewrite_json_version("{\n  \"version\": \"0.5.9\",\n  \"x\": 1\n}", "0.8.0").unwrap();
        assert!(out.contains("\"version\": \"0.8.0\""));
        assert!(!out.contains("0.5.9"));
        assert!(out.contains("\"x\": 1"), "other keys preserved");
    }

    #[test]
    fn scoop_errors_without_version_key() {
        assert!(rewrite_json_version("{}", "1.0").is_err());
    }

    #[test]
    fn pkgbuild_features_are_dist_channels() {
        let f = spec::resolve_feature_list(&root(), &Selection::Dist)
            .unwrap()
            .join(",");
        assert!(f.contains("channel-discord") && !f.contains("hardware"));
    }
}
