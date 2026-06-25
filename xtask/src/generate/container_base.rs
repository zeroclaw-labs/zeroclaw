use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

const SOURCE: &str = "dev/ci/container-base-images.toml";
const NODE_SUITE: &str = "bookworm-slim";
const INDEX_ACCEPT: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Registry {
    DockerHub,
    Gcr,
}

impl Registry {
    fn host(self) -> &'static str {
        match self {
            Registry::DockerHub => "registry-1.docker.io",
            Registry::Gcr => "gcr.io",
        }
    }

    fn token_url(self, repo: &str) -> String {
        match self {
            Registry::DockerHub => format!(
                "https://auth.docker.io/token?service=registry.docker.io&scope=repository:{repo}:pull"
            ),
            Registry::Gcr => {
                format!("https://gcr.io/v2/token?service=gcr.io&scope=repository:{repo}:pull")
            }
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct BaseImage {
    zone: String,
    arg: String,
    registry: Registry,
    repo: String,
    image_ref: String,
    #[serde(default, skip_serializing_if = "is_false")]
    discover: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    digest: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Serialize, Deserialize)]
struct Source {
    image: Vec<BaseImage>,
}

fn load(root: &Path) -> anyhow::Result<Source> {
    let raw =
        std::fs::read_to_string(root.join(SOURCE)).with_context(|| format!("read {SOURCE}"))?;
    toml::from_str(&raw).with_context(|| format!("parse {SOURCE}"))
}

fn client() -> anyhow::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("build http client")
}

#[derive(Deserialize)]
struct Token {
    token: String,
}

fn node_major_for_suite(tag: &str) -> Option<u32> {
    tag.strip_suffix(NODE_SUITE)
        .and_then(|tag| tag.strip_suffix('-'))
        .filter(|m| !m.is_empty() && m.chars().all(|c| c.is_ascii_digit()))
        .and_then(|m| m.parse::<u32>().ok())
}

fn resolve_digest(
    client: &reqwest::blocking::Client,
    registry: Registry,
    repo: &str,
    tag: &str,
) -> anyhow::Result<String> {
    let token: Token = client
        .get(registry.token_url(repo))
        .send()
        .with_context(|| format!("{repo}: registry auth request"))?
        .error_for_status()
        .with_context(|| format!("{repo}: registry auth status"))?
        .json()
        .with_context(|| format!("{repo}: parse registry token"))?;
    let resp = client
        .head(format!(
            "https://{}/v2/{repo}/manifests/{tag}",
            registry.host()
        ))
        .bearer_auth(&token.token)
        .header(reqwest::header::ACCEPT, INDEX_ACCEPT)
        .send()
        .with_context(|| format!("{repo}:{tag}: manifest head request"))?
        .error_for_status()
        .with_context(|| format!("{repo}:{tag}: manifest head status"))?;
    let digest = resp
        .headers()
        .get("docker-content-digest")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .with_context(|| format!("{repo}:{tag}: registry returned no Docker-Content-Digest"))?;
    if !valid_digest(&digest) {
        anyhow::bail!("{repo}:{tag}: unexpected digest form: {digest}");
    }
    Ok(digest)
}

fn valid_digest(digest: &str) -> bool {
    digest
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

fn arg_line(img: &BaseImage, tag: &str, digest: &str) -> String {
    format!("ARG {}={}:{}@{}", img.arg, img.image_ref, tag, digest)
}

fn begin(zone: &str) -> String {
    format!("# >>> generated:{zone} from {SOURCE} by `cargo generate installers` - do not edit <<<")
}

fn end(zone: &str) -> String {
    format!("# >>> end generated:{zone} <<<")
}

fn zone_body(current: &str, zone: &str) -> Option<String> {
    let b = begin(zone);
    let e = end(zone);
    let start = current.find(&b)? + b.len();
    let rel = current[start..].find(&e)?;
    Some(current[start..start + rel].trim().to_string())
}

fn splice(current: &str, zone: &str, body: &str) -> anyhow::Result<String> {
    let b = begin(zone);
    let e = end(zone);
    let begin_at = current
        .find(&b)
        .with_context(|| format!("missing generated:{zone} BEGIN sentinel"))?;
    let after_begin = begin_at + b.len();
    let end_rel = current[after_begin..]
        .find(&e)
        .with_context(|| format!("missing generated:{zone} END sentinel"))?;
    let end_at = after_begin + end_rel;
    Ok(format!(
        "{}\n{}\n{}",
        &current[..after_begin],
        body,
        &current[end_at..]
    ))
}

/// Resolve every row live and persist tag+digest back into the canonical TOML so
/// it stays the single source the surfaces render from.
pub fn refresh_source(root: &Path) -> anyhow::Result<()> {
    let client = client()?;
    let mut src = load(root)?;
    for img in &mut src.image {
        let tag = img
            .tag
            .clone()
            .with_context(|| format!("{}: row must set a tag", img.zone))?;
        validate_node_tag_policy(img, &tag)?;
        let digest = resolve_digest(&client, img.registry, &img.repo, &tag)?;
        img.tag = Some(tag);
        img.digest = Some(digest);
    }
    let rendered = render_source(&src)?;
    std::fs::write(root.join(SOURCE), rendered).with_context(|| format!("write {SOURCE}"))?;
    Ok(())
}

const SOURCE_HEADER: &str = "# Canonical container base-image pins for the generated container surfaces\n\
# (Dockerfile, Dockerfile.debian). Edit registry/repo/image_ref/tag here; `tag`\n\
# records the intended policy and `digest` is rewritten live by `cargo generate\n\
# installers`. A row with discover=true refreshes the digest for its declared\n\
# tag from the registry; Node discover rows must use a plain\n\
# <major>-bookworm-slim tag. StageX pins in the Containerfile are excluded on\n\
# purpose: digest-only, reproducible-build intent, no tag to follow.\n\n";

fn render_source(src: &Source) -> anyhow::Result<String> {
    let body = toml::to_string_pretty(src).context("serialize source")?;
    Ok(format!("{SOURCE_HEADER}{body}"))
}

/// Splice the ARG zones each surface declares, sourced from the canonical TOML.
/// Network-free: the TOML already carries the resolved tag+digest.
pub fn splice_zones(root: &Path, current: &str) -> anyhow::Result<String> {
    let src = load(root)?;
    let mut out = current.to_string();
    for img in &src.image {
        if !current.contains(&begin(&img.zone)) {
            continue;
        }
        let (tag, digest) = resolved(img)?;
        out = splice(&out, &img.zone, &arg_line(img, tag, digest))?;
    }
    Ok(out)
}

fn resolved(img: &BaseImage) -> anyhow::Result<(&str, &str)> {
    let tag = img.tag.as_deref().with_context(|| {
        format!(
            "{}: TOML missing resolved tag; run `cargo generate installers`",
            img.zone
        )
    })?;
    validate_node_tag_policy(img, tag)?;
    let digest = img.digest.as_deref().with_context(|| {
        format!(
            "{}: TOML missing resolved digest; run `cargo generate installers`",
            img.zone
        )
    })?;
    Ok((tag, digest))
}

fn validate_node_tag_policy(img: &BaseImage, tag: &str) -> anyhow::Result<()> {
    if !img.discover {
        return Ok(());
    }
    node_major_for_suite(tag)
        .with_context(|| format!("{}: node tag {tag} is not <major>-{NODE_SUITE}", img.zone))?;
    Ok(())
}

/// Network-free drift check: every declared ARG zone must match what the TOML
/// says, the TOML must carry a resolved tag+digest, and node's tag must be a
/// plain-major LTS-suite tag. A `FROM ${ARG}` with no zone is flagged.
pub fn check(root: &Path, current: &str) -> anyhow::Result<Vec<String>> {
    let src = load(root)?;
    let mut drift = Vec::new();
    for img in &src.image {
        let references = current.contains(&format!("${{{}}}", img.arg));
        let declares = current.contains(&begin(&img.zone));
        if references && !declares {
            drift.push(format!(
                "{} (FROM references ${} but zone is gone)",
                img.zone, img.arg
            ));
            continue;
        }
        if !declares {
            continue;
        }
        let (tag, digest) = match resolved(img) {
            Ok(v) => v,
            Err(e) => {
                drift.push(e.to_string());
                continue;
            }
        };
        if !valid_digest(digest) {
            drift.push(format!("{}: malformed digest {digest}", img.zone));
        }
        match zone_body(current, &img.zone) {
            Some(body) if body == arg_line(img, tag, digest) => {}
            _ => drift.push(img.zone.clone()),
        }
    }
    Ok(drift)
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

    fn source_with_node_tag(tag: &str) -> String {
        format!(
            r#"[[image]]
zone = "base-arg-node"
arg = "ZEROCLAW_BASE_NODE"
registry = "dockerhub"
repo = "library/node"
image_ref = "node"
discover = true
tag = "{tag}"
digest = "sha256:{}"
"#,
            "a".repeat(64)
        )
    }

    fn write_source(root: &Path, source: &str) {
        let dir = root.join("dev/ci");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("container-base-images.toml"), source).unwrap();
    }

    fn fixed(zone: &str) -> BaseImage {
        BaseImage {
            zone: zone.to_string(),
            arg: "ZEROCLAW_TEST".to_string(),
            registry: Registry::DockerHub,
            repo: "library/rust".to_string(),
            image_ref: "rust".to_string(),
            discover: false,
            tag: Some("1.94-slim".to_string()),
            digest: Some(format!("sha256:{}", "a".repeat(64))),
        }
    }

    #[test]
    fn source_parses_and_node_is_discover() {
        let src = load(&root()).unwrap();
        let node = src
            .image
            .iter()
            .find(|i| i.zone == "base-arg-node")
            .unwrap();
        assert!(node.discover, "node tag must be discovered live");
        assert!(src.image.iter().any(|i| i.zone == "base-arg-rust-slim"));
    }

    #[test]
    fn node_major_for_suite_accepts_plain_major() {
        assert_eq!(node_major_for_suite("24-bookworm-slim"), Some(24));
        assert_eq!(node_major_for_suite("26-bookworm-slim"), Some(26));
    }

    #[test]
    fn node_major_for_suite_rejects_non_plain_major() {
        assert_eq!(node_major_for_suite("24.1-bookworm-slim"), None);
        assert_eq!(node_major_for_suite("lts-bookworm-slim"), None);
        assert_eq!(node_major_for_suite("24-alpine"), None);
        assert_eq!(node_major_for_suite("-bookworm-slim"), None);
    }

    #[test]
    fn valid_digest_shape() {
        assert!(valid_digest(&format!("sha256:{}", "a".repeat(64))));
        assert!(!valid_digest(&format!("sha256:{}", "a".repeat(10))));
        assert!(!valid_digest("md5:abc"));
    }

    #[test]
    fn arg_line_round_trips_through_zone_body() {
        let img = fixed("base-arg-test");
        let (tag, digest) = resolved(&img).unwrap();
        let line = arg_line(&img, tag, digest);
        let content = format!("{}\n{line}\n{}\n", begin(&img.zone), end(&img.zone));
        assert_eq!(zone_body(&content, &img.zone).unwrap(), line);
    }

    #[test]
    fn check_flags_orphan_reference() {
        let content = "FROM ${ZEROCLAW_BASE_RUST_SLIM} AS x\n";
        let drift = check(&root(), content).unwrap();
        assert!(drift.iter().any(|d| d.contains("base-arg-rust-slim")));
    }

    #[test]
    fn splice_zones_rejects_non_plain_node_discover_tag() {
        let temp = tempfile::tempdir().unwrap();
        write_source(temp.path(), &source_with_node_tag("24.1-bookworm-slim"));
        let content = format!(
            "{}\nARG ZEROCLAW_BASE_NODE=node:24.1-bookworm-slim@sha256:{}\n{}\n",
            begin("base-arg-node"),
            "a".repeat(64),
            end("base-arg-node")
        );

        let err = splice_zones(temp.path(), &content).unwrap_err();

        assert!(
            err.to_string()
                .contains("node tag 24.1-bookworm-slim is not <major>-bookworm-slim"),
            "expected Node tag-shape error, got {err:#}"
        );
    }
}
