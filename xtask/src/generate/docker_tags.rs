//! Docker tag matrix generator. Emits the canonical `(tag, selection,
//! dockerfile)` table the docker-publish workflow consumes — Alpine-style
//! version-pinned tags plus floating tags. Both the version and the per-tag
//! feature selection come from the spec; nothing is typed. Output is JSON
//! (consumed by CI), fully generated (the whole file is owned).

use super::spec::{self, Selection};
use std::path::Path;

/// A published image tag bound to a selection and the dockerfile that builds it.
struct TagSpec {
    /// Tag stem (the selection id, e.g. "dist", "all-features").
    stem: &'static str,
    selection: Selection,
    dockerfile: &'static str,
}

/// The canonical tag set. Each selection maps to a stem + the dockerfile that
/// realizes it. Driven by selections, not literal tag strings.
fn tag_specs() -> Vec<TagSpec> {
    vec![
        TagSpec {
            stem: "minimal",
            selection: Selection::Minimal,
            dockerfile: "Dockerfile",
        },
        TagSpec {
            stem: "default-features",
            selection: Selection::Full,
            dockerfile: "Dockerfile",
        },
        TagSpec {
            stem: "dist",
            selection: Selection::Dist,
            dockerfile: "Dockerfile",
        },
        TagSpec {
            stem: "all-features",
            selection: Selection::All,
            dockerfile: "Containerfile",
        },
    ]
}

/// Render the docker tag matrix JSON: for each selection, a pinned tag
/// (`<stem>-v<version>`, immutable, Alpine-style) and a floating tag (`<stem>`,
/// re-points to latest), with the resolved feature list and dockerfile.
pub fn render(root: &Path) -> anyhow::Result<String> {
    let version = spec::resolve_version(root)?;
    let mut entries = Vec::new();
    for ts in tag_specs() {
        let features = spec::resolve_feature_list(root, &ts.selection)?.join(",");
        entries.push(format!(
            "    {{\n      \"stem\": \"{stem}\",\n      \"pinned_tag\": \"{stem}-v{version}\",\n      \"floating_tag\": \"{stem}\",\n      \"dockerfile\": \"{df}\",\n      \"features\": \"{features}\"\n    }}",
            stem = ts.stem,
            df = ts.dockerfile,
        ));
    }
    Ok(format!(
        "{{\n  \"version\": \"{version}\",\n  \"tags\": [\n{}\n  ]\n}}\n",
        entries.join(",\n")
    ))
}

/// Render-or-check: the matrix is a fully generated file (no hand-written glue),
/// so the renderer ignores prior content and emits the whole document.
pub fn render_file(root: &Path, _current: &str) -> anyhow::Result<String> {
    render(root)
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
    fn matrix_is_valid_json_with_pinned_and_floating() {
        let s = render(&root()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).expect("valid JSON");
        let tags = v["tags"].as_array().unwrap();
        assert_eq!(tags.len(), tag_specs().len());
        let ver = v["version"].as_str().unwrap();
        // Pinned tag is Alpine-style stem-v<version>; floating is the bare stem.
        let dist = tags.iter().find(|t| t["stem"] == "dist").unwrap();
        assert_eq!(dist["pinned_tag"], format!("dist-v{ver}"));
        assert_eq!(dist["floating_tag"], "dist");
    }

    #[test]
    fn dist_tag_ships_channels_all_tag_ships_heavyweight() {
        let s = render(&root()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        let tags = v["tags"].as_array().unwrap();
        let dist = tags.iter().find(|t| t["stem"] == "dist").unwrap();
        let all = tags.iter().find(|t| t["stem"] == "all-features").unwrap();
        assert!(
            dist["features"]
                .as_str()
                .unwrap()
                .contains("channel-discord")
        );
        assert!(!dist["features"].as_str().unwrap().contains("hardware"));
        assert!(all["features"].as_str().unwrap().contains("hardware"));
    }
}
