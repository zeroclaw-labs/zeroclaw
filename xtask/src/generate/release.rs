//! Published CLI selections come from Cargo metadata; the release workflow
//! remains the canonical target matrix. On-demand builds do not multiply it.

use super::{container, spec};
use std::path::Path;

pub fn render_file(root: &Path, current: &str) -> anyhow::Result<String> {
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(root.join("Cargo.toml"))
        .no_deps()
        .exec()?;
    let selections = spec::release_distributions(spec::workspace_root_package(&metadata)?)?;
    let body = format!(
        "        distribution: {}",
        serde_json::to_string(&selections)?
    );
    container::splice(current, "release-distributions", &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    #[test]
    fn published_cli_matrix_is_ten_target_legs_with_stable_names() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let current =
            std::fs::read_to_string(root.join(".github/workflows/release-stable-manual.yml"))
                .unwrap();
        let rendered = render_file(&root, &current).unwrap();
        assert_eq!(rendered, current, "release policy must not drift");
        let build = rendered
            .split_once("\n  build:\n")
            .unwrap()
            .1
            .split_once("\n  build-desktop:\n")
            .unwrap()
            .0;
        let targets: Vec<_> = build
            .lines()
            .find_map(|line| line.trim().strip_prefix("target: ["))
            .unwrap()
            .trim_end_matches(']')
            .split(',')
            .map(str::trim)
            .collect();
        let selections: Vec<String> = serde_json::from_str(
            build
                .lines()
                .find_map(|line| line.trim().strip_prefix("distribution: "))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(selections, ["dist"]);
        assert_eq!(targets.len() * selections.len(), 10);
        assert_eq!(targets.iter().collect::<BTreeSet<_>>().len(), 10);
        let included: BTreeSet<_> = build
            .lines()
            .filter_map(|line| line.strip_prefix("            target: "))
            .collect();
        assert_eq!(included, targets.iter().copied().collect());
        assert!(!build.contains("dist-compat"));
        assert!(!build.contains("matrix.suffix"));
        assert!(build.contains("name: zeroclaw-${{ matrix.target }}"));
        assert!(build.contains(
            "features --selection \"${{ matrix.distribution }}\" --target \"${{ matrix.target }}\""
        ));
    }
}
