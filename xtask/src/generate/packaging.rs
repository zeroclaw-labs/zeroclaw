//! Packaging surface renderers: AUR PKGBUILD (shell-comment sentinels) and the
//! scoop manifest (JSON, no comments - targeted key rewrites). Version and
//! feature sets come from the canonical spec; nothing is typed.

use super::spec::{self, Selection};
use std::path::Path;

fn begin(zone: &str) -> String {
    format!("# >>> generated:{zone} by `cargo generate installers` - do not edit <<<")
}
fn end(zone: &str) -> String {
    format!("# >>> end generated:{zone} <<<")
}

fn splice(current: &str, zone: &str, body: &str) -> anyhow::Result<String> {
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

/// PKGBUILD: regenerate the `pkgver` and the build `--features` from the spec.
/// Ships `Selection::Dist`. Two zones - version and the cargo build line.
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

/// Scoop manifest is JSON (no comments). Rewrite the top-level `"version"` and
/// materialize the primary download URL from the canonical autoupdate template,
/// preserving formatting everywhere else.
pub fn render_scoop(root: &Path, current: &str) -> anyhow::Result<String> {
    let version = spec::resolve_version(root)?;
    rewrite_scoop_release_fields(current, &version)
}

fn rewrite_scoop_release_fields(current: &str, version: &str) -> anyhow::Result<String> {
    let manifest: serde_json::Value = serde_json::from_str(current)
        .map_err(|error| anyhow::Error::msg(format!("invalid scoop manifest JSON: {error}")))?;
    let primary_url = manifest
        .pointer("/architecture/64bit/url")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::Error::msg("scoop manifest has no 64-bit download URL"))?;
    let autoupdate_url = manifest
        .pointer("/autoupdate/architecture/64bit/url")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::Error::msg("scoop manifest has no 64-bit autoupdate URL"))?;
    if !autoupdate_url.contains("$version") {
        return Err(anyhow::Error::msg(
            "scoop 64-bit autoupdate URL has no $version placeholder",
        ));
    }

    let release_url = autoupdate_url.replace("$version", version);
    let with_version = rewrite_json_version(current, version)?;
    rewrite_json_string_once(&with_version, primary_url, &release_url)
}

fn rewrite_json_version(current: &str, version: &str) -> anyhow::Result<String> {
    // Match the first `"version": "..."` and replace its value.
    let key = "\"version\":";
    let key_at = current
        .find(key)
        .ok_or_else(|| anyhow::Error::msg("scoop manifest has no \"version\" key"))?;
    let after_key = key_at + key.len();
    let rest = &current[after_key..];
    let q1 = rest
        .find('"')
        .ok_or_else(|| anyhow::Error::msg("malformed version value"))?;
    let q2 = rest[q1 + 1..]
        .find('"')
        .ok_or_else(|| anyhow::Error::msg("unterminated version value"))?;
    let val_start = after_key + q1 + 1;
    let val_end = after_key + q1 + 1 + q2;
    let mut out = String::new();
    out.push_str(&current[..val_start]);
    out.push_str(version);
    out.push_str(&current[val_end..]);
    Ok(out)
}

fn rewrite_json_string_once(
    current: &str,
    old_value: &str,
    new_value: &str,
) -> anyhow::Result<String> {
    let old_json = serde_json::to_string(old_value)?;
    let new_json = serde_json::to_string(new_value)?;
    let mut matches = current.match_indices(&old_json);
    let (value_at, _) = matches
        .next()
        .ok_or_else(|| anyhow::Error::msg("scoop manifest value was not found verbatim"))?;
    if matches.next().is_some() {
        return Err(anyhow::Error::msg(
            "scoop manifest value is ambiguous and cannot be rewritten safely",
        ));
    }

    let value_end = value_at + old_json.len();
    let mut out = String::new();
    out.push_str(&current[..value_at]);
    out.push_str(&new_json);
    out.push_str(&current[value_end..]);
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
    fn scoop_release_fields_are_rewritten_from_autoupdate_url() {
        let current = r#"{
  "version": "0.5.9",
  "architecture": {
    "64bit": {
      "url": "https://example.test/releases/download/v0.5.9/zeroclaw.zip"
    }
  },
  "autoupdate": {
    "architecture": {
      "64bit": {
        "url": "https://example.test/releases/download/v$version/zeroclaw.zip"
      }
    }
  },
  "x": 1
}"#;
        let out = rewrite_scoop_release_fields(current, "0.8.0").unwrap();
        assert!(out.contains("\"version\": \"0.8.0\""));
        assert!(!out.contains("0.5.9"));
        assert!(out.contains("releases/download/v0.8.0/zeroclaw.zip"));
        assert!(out.contains("releases/download/v$version/zeroclaw.zip"));
        assert!(out.contains("\"x\": 1"), "other keys preserved");
    }

    #[test]
    fn scoop_errors_without_version_key() {
        let current = r#"{
  "architecture": {"64bit": {"url": "https://example.test/v0.5.9/zeroclaw.zip"}},
  "autoupdate": {"architecture": {"64bit": {"url": "https://example.test/v$version/zeroclaw.zip"}}}
}"#;
        assert!(rewrite_scoop_release_fields(current, "1.0").is_err());
    }

    #[test]
    fn scoop_errors_without_autoupdate_version_placeholder() {
        let current = r#"{
  "version": "0.5.9",
  "architecture": {"64bit": {"url": "https://example.test/v0.5.9/zeroclaw.zip"}},
  "autoupdate": {"architecture": {"64bit": {"url": "https://example.test/latest/zeroclaw.zip"}}}
}"#;
        assert!(rewrite_scoop_release_fields(current, "1.0").is_err());
    }

    #[test]
    fn pkgbuild_features_are_lean_dist_channels() {
        let f = spec::resolve_feature_list(&root(), &Selection::Dist)
            .unwrap()
            .join(",");
        assert!(f.contains("channel-matrix"));
        assert!(f.contains("whatsapp-web"));
        assert!(!f.contains("channel-slack"));
    }
}
