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

/// AUR .SRCINFO: render every package field from the checked-in PKGBUILD.
/// The parser accepts only the deliberately simple assignment grammar used by
/// this package, so an unsupported dynamic expression fails closed in CI.
pub fn render_srcinfo(root: &Path, _current: &str) -> anyhow::Result<String> {
    let version = spec::resolve_version(root)?;
    let pkgbuild_path = root.join("dist/aur/PKGBUILD");
    let pkgbuild = std::fs::read_to_string(&pkgbuild_path).map_err(|error| {
        anyhow::Error::msg(format!(
            "failed to read {}: {error}",
            pkgbuild_path.display()
        ))
    })?;
    render_srcinfo_from_pkgbuild(&version, &pkgbuild)
}

fn assignment<'a>(pkgbuild: &'a str, key: &str) -> anyhow::Result<Option<&'a str>> {
    let prefix = format!("{key}=");
    let values = pkgbuild
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix))
        .collect::<Vec<_>>();
    if values.len() > 1 {
        anyhow::bail!("expected at most one `{key}` assignment in AUR PKGBUILD");
    }
    Ok(values.first().copied())
}

fn validate_top_level_assignments(pkgbuild: &str) -> anyhow::Result<()> {
    const KNOWN: &[&str] = &[
        "pkgname",
        "pkgver",
        "pkgrel",
        "epoch",
        "pkgdesc",
        "arch",
        "url",
        "license",
        "depends",
        "makedepends",
        "provides",
        "conflicts",
        "source",
        "sha256sums",
    ];

    for line in pkgbuild.lines() {
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        if key.is_empty()
            || !key
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            continue;
        }
        if !key.starts_with('_') && !KNOWN.contains(&key) {
            anyhow::bail!("unsupported top-level AUR PKGBUILD assignment `{key}`");
        }
    }
    Ok(())
}

fn reject_dynamic_value(key: &str, value: &str) -> anyhow::Result<()> {
    if value.contains('$') || value.contains('`') {
        anyhow::bail!("dynamic `{key}` AUR PKGBUILD values are not supported");
    }
    Ok(())
}

fn scalar(pkgbuild: &str, key: &str) -> anyhow::Result<String> {
    let raw = assignment(pkgbuild, key)?
        .ok_or_else(|| anyhow::Error::msg(format!("missing `{key}` in AUR PKGBUILD")))?;
    if raw.len() >= 2 {
        let first = raw.as_bytes()[0];
        let last = raw.as_bytes()[raw.len() - 1];
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            let value = &raw[1..raw.len() - 1];
            if value.as_bytes().contains(&first) {
                anyhow::bail!("unsupported quote in `{key}` AUR PKGBUILD assignment");
            }
            reject_dynamic_value(key, value)?;
            return Ok(value.to_owned());
        }
    }
    if raw.is_empty() || raw.chars().any(char::is_whitespace) {
        anyhow::bail!("unsupported scalar `{key}` AUR PKGBUILD assignment");
    }
    reject_dynamic_value(key, raw)?;
    Ok(raw.to_owned())
}

fn array(pkgbuild: &str, key: &str) -> anyhow::Result<Vec<String>> {
    let raw = assignment(pkgbuild, key)?
        .ok_or_else(|| anyhow::Error::msg(format!("missing `{key}` in AUR PKGBUILD")))?;
    let inner = raw
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .ok_or_else(|| anyhow::Error::msg(format!("`{key}` must be a one-line quoted array")))?;
    let mut values = Vec::new();
    let mut rest = inner.trim();
    while !rest.is_empty() {
        let quote = rest.as_bytes()[0];
        if quote != b'\'' && quote != b'"' {
            anyhow::bail!("`{key}` array entries must be quoted");
        }
        let tail = &rest[1..];
        let end = tail
            .bytes()
            .position(|byte| byte == quote)
            .ok_or_else(|| anyhow::Error::msg(format!("unterminated `{key}` array entry")))?;
        let value = &tail[..end];
        if value.contains('\\') {
            anyhow::bail!("escaped `{key}` array entries are not supported");
        }
        if key != "source" {
            reject_dynamic_value(key, value)?;
        }
        values.push(value.to_owned());
        rest = tail[end + 1..].trim_start();
    }
    if values.is_empty() {
        anyhow::bail!("`{key}` AUR PKGBUILD array must not be empty");
    }
    Ok(values)
}

fn push_srcinfo_values(rendered: &mut String, key: &str, values: &[String]) {
    for value in values {
        rendered.push_str(&format!("\t{key} = {value}\n"));
    }
}

fn render_srcinfo_from_pkgbuild(version: &str, pkgbuild: &str) -> anyhow::Result<String> {
    validate_top_level_assignments(pkgbuild)?;
    let pkgname = scalar(pkgbuild, "pkgname")?;
    let pkgrel = scalar(pkgbuild, "pkgrel")?;
    let pkgdesc = scalar(pkgbuild, "pkgdesc")?;
    let url = scalar(pkgbuild, "url")?;
    let epoch = assignment(pkgbuild, "epoch")?
        .map(|_| scalar(pkgbuild, "epoch"))
        .transpose()?;
    let arch = array(pkgbuild, "arch")?;
    let license = array(pkgbuild, "license")?;
    let makedepends = array(pkgbuild, "makedepends")?;
    let depends = array(pkgbuild, "depends")?;
    let provides = array(pkgbuild, "provides")?;
    let conflicts = array(pkgbuild, "conflicts")?;
    let sources = array(pkgbuild, "source")?
        .into_iter()
        .map(|source| {
            let expanded = source
                .replace("${pkgname}", &pkgname)
                .replace("${pkgver}", version);
            reject_dynamic_value("source", &expanded)?;
            Ok(expanded)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let checksums = array(pkgbuild, "sha256sums")?;
    if sources.len() != checksums.len() {
        anyhow::bail!("AUR PKGBUILD source and sha256sums counts differ");
    }

    let mut rendered = format!(
        "pkgbase = {pkgname}\n\tpkgdesc = {pkgdesc}\n\tpkgver = {version}\n\tpkgrel = {pkgrel}\n"
    );
    if let Some(epoch) = epoch {
        rendered.push_str(&format!("\tepoch = {epoch}\n"));
    }
    rendered.push_str(&format!("\turl = {url}\n"));
    push_srcinfo_values(&mut rendered, "arch", &arch);
    push_srcinfo_values(&mut rendered, "license", &license);
    push_srcinfo_values(&mut rendered, "makedepends", &makedepends);
    push_srcinfo_values(&mut rendered, "depends", &depends);
    push_srcinfo_values(&mut rendered, "provides", &provides);
    push_srcinfo_values(&mut rendered, "conflicts", &conflicts);
    push_srcinfo_values(&mut rendered, "source", &sources);
    push_srcinfo_values(&mut rendered, "sha256sums", &checksums);
    rendered.push_str(&format!("\npkgname = {pkgname}\n"));
    Ok(rendered)
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
    fn srcinfo_is_a_complete_projection_of_pkgbuild() {
        let version = spec::resolve_version(&root()).unwrap();
        let pkgbuild = std::fs::read_to_string(root().join("dist/aur/PKGBUILD")).unwrap();
        let rendered = render_srcinfo_from_pkgbuild(&version, &pkgbuild).unwrap();
        assert!(rendered.contains(&format!("\tpkgver = {version}\n")));
        assert!(rendered.contains(&format!(
            "\tsource = zeroclawlabs-{version}.tar.gz::https://github.com/zeroclaw-labs/zeroclaw/archive/refs/tags/v{version}.tar.gz\n"
        )));
        assert_eq!(
            rendered,
            std::fs::read_to_string(root().join("dist/aur/.SRCINFO")).unwrap()
        );

        let changed = pkgbuild.replace(
            "depends=('gcc-libs' 'openssl')",
            "depends=('gcc-libs' 'openssl' 'sqlite')",
        );
        let changed_srcinfo = render_srcinfo_from_pkgbuild(&version, &changed).unwrap();
        assert!(changed_srcinfo.contains("\tdepends = sqlite\n"));
        assert_ne!(changed_srcinfo, rendered);
    }

    #[test]
    fn srcinfo_renderer_rejects_ambiguous_or_dynamic_pkgbuild_metadata() {
        let version = spec::resolve_version(&root()).unwrap();
        let pkgbuild = std::fs::read_to_string(root().join("dist/aur/PKGBUILD")).unwrap();
        let duplicate = format!("{pkgbuild}\ndepends=('other')\n");
        assert!(render_srcinfo_from_pkgbuild(&version, &duplicate).is_err());
        let dynamic = pkgbuild.replace("depends=('gcc-libs' 'openssl')", "depends=($EXTRA_DEPS)");
        assert!(render_srcinfo_from_pkgbuild(&version, &dynamic).is_err());
        let dynamic_scalar = pkgbuild.replace("pkgrel=1", "pkgrel=$REL");
        assert!(render_srcinfo_from_pkgbuild(&version, &dynamic_scalar).is_err());
        let unknown = format!("{pkgbuild}\noptdepends=('sqlite: optional database support')\n");
        assert!(render_srcinfo_from_pkgbuild(&version, &unknown).is_err());

        let with_epoch = pkgbuild.replace("pkgver=", "epoch=2\npkgver=");
        let rendered = render_srcinfo_from_pkgbuild(&version, &with_epoch).unwrap();
        assert!(rendered.contains("\tepoch = 2\n"));
        assert!(rendered.find("\tpkgrel = 1\n").unwrap() < rendered.find("\tepoch = 2\n").unwrap());
        assert!(rendered.find("\tepoch = 2\n").unwrap() < rendered.find("\turl = ").unwrap());
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
        for feature in spec::resolve_feature_list(&root(), &Selection::Dist).unwrap() {
            assert!(f.contains(&feature), "dist feature {feature} not rendered");
        }
        for feature in spec::features_outside_dist(&root()).unwrap() {
            assert!(!f.contains(&feature), "{feature} leaked into lean dist");
        }
    }
}
