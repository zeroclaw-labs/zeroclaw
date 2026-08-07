//! Architecture gates for release-workflow artifact identity.
//!
//! The macOS desktop job must notarize, staple, validate, and upload the same
//! DMG. Discovering the file independently in multiple steps can notarize one
//! image while publishing another.

use std::fs;
use std::path::Path;
use std::process::Command;

fn workflow(name: &str) -> String {
    let workflow_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".github/workflows")
        .join(name);
    fs::read_to_string(&workflow_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", workflow_path.display()))
}

#[test]
fn macos_desktop_release_notarizes_published_dmg() {
    let workflow_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release-stable-manual.yml");
    let workflow = fs::read_to_string(&workflow_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", workflow_path.display()));
    let macos_job = workflow
        .split_once("  build-desktop:\n")
        .and_then(|(_, remainder)| remainder.split_once("  # New desktop platforms."))
        .map(|(job, _)| job)
        .expect("release workflow must contain the macOS desktop build job");

    assert_eq!(
        macos_job.matches("MACOS_DMG_PATH:").count(),
        1,
        "the published DMG path must have exactly one source of truth"
    );
    for required in [
        "MACOS_DMG_PATH: desktop-assets/ZeroClaw.dmg",
        "dmg_dir=\"target/universal-apple-darwin/release/bundle/dmg\"",
        "dmg_candidates=(\"$dmg_dir\"/*.dmg)",
        "\"${#dmg_candidates[@]}\" -ne 1",
        "mv \"${dmg_candidates[0]}\" \"$MACOS_DMG_PATH\"",
        "notarytool submit \"$MACOS_DMG_PATH\"",
        "stapler staple \"$MACOS_DMG_PATH\"",
        "stapler validate \"$MACOS_DMG_PATH\"",
        "${{ env.MACOS_DMG_PATH }}",
    ] {
        assert!(
            macos_job.contains(required),
            "macOS desktop job is missing release invariant: {required}"
        );
    }

    assert!(
        !macos_job.contains("find target -name '*.dmg'"),
        "the macOS desktop job must not rediscover DMGs from the whole target tree"
    );

    let positions = [
        "mv \"${dmg_candidates[0]}\" \"$MACOS_DMG_PATH\"",
        "notarytool submit \"$MACOS_DMG_PATH\"",
        "stapler staple \"$MACOS_DMG_PATH\"",
        "stapler validate \"$MACOS_DMG_PATH\"",
        "uses: actions/upload-artifact@",
    ]
    .map(|needle| {
        macos_job
            .find(needle)
            .unwrap_or_else(|| panic!("macOS desktop job is missing ordered step: {needle}"))
    });
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "the final DMG must be prepared, notarized, stapled, validated, then uploaded"
    );
}

#[test]
fn package_publishers_use_canonical_sources_and_scoped_credentials() {
    let release = workflow("release-stable-manual.yml");
    assert!(
        !release.contains("pub-homebrew-core.yml"),
        "Homebrew Core is updated by its official autobump service, not a duplicate publisher"
    );
    assert!(
        !Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".github/workflows/pub-homebrew-core.yml")
            .exists(),
        "the redundant project-owned Homebrew publisher must stay retired"
    );

    let scoop = workflow("pub-scoop.yml");
    for required in [
        "SCOOP_BUCKET_TOKEN",
        "dist/scoop/zeroclaw.json",
        "push --dry-run origin HEAD",
        "Contents: Read and write",
        ".architecture[\"64bit\"].url = $url",
        ".architecture[\"64bit\"].hash = $hash",
    ] {
        assert!(
            scoop.contains(required),
            "Scoop publisher is missing packaging invariant: {required}"
        );
    }
    for forbidden in [
        "gh api \"repos/${SCOOP_BUCKET_REPO}\" --jq '.permissions.push'",
        "cat > \"$manifest_file\" <<MANIFEST",
    ] {
        assert!(
            !scoop.contains(forbidden),
            "Scoop publisher must not contain duplicate or heuristic path: {forbidden}"
        );
    }

    assert!(
        scoop.contains(
            "bash scripts/release/scoop_metadata.sh dist/scoop/zeroclaw.json \"$version\""
        ),
        "pub-scoop.yml must materialize publisher metadata from the canonical manifest"
    );
    assert!(
        !scoop.contains("https://github.com/${GITHUB_REPOSITORY}/releases/download/"),
        "pub-scoop.yml must not rebuild a release URL independently of the canonical manifest"
    );

    let aur = workflow("pub-aur.yml");
    assert!(
        !aur.contains("ssh -T -o"),
        "AUR clone/push is the authoritative authentication check"
    );
}

#[test]
fn scoop_publisher_metadata_follows_canonical_url_template() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let metadata_script = root.join("scripts/release/scoop_metadata.sh");
    let script = fs::read_to_string(&metadata_script)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", metadata_script.display()));
    assert!(
        !script.contains("eval "),
        "canonical Scoop URL templates must never be evaluated as shell code"
    );

    let temp = tempfile::tempdir().expect("create temporary Scoop manifest directory");
    let manifest_path = temp.path().join("zeroclaw.json");
    fs::write(
        &manifest_path,
        r#"{
  "autoupdate": {
    "architecture": {
      "64bit": {
        "url": "https://downloads.example.test/renamed/repository/releases/v$version/zeroclaw-renamed.zip"
      }
    }
  }
}"#,
    )
    .expect("write temporary Scoop manifest");

    let output = Command::new("bash")
        .arg(&metadata_script)
        .arg(&manifest_path)
        .arg("1.2.3")
        .output()
        .expect("run Scoop metadata materializer");
    assert!(
        output.status.success(),
        "Scoop metadata materializer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse Scoop publisher metadata");

    assert_eq!(
        metadata["zip_url"],
        "https://downloads.example.test/renamed/repository/releases/v1.2.3/zeroclaw-renamed.zip"
    );
    assert_eq!(metadata["asset_name"], "zeroclaw-renamed.zip");
    assert_eq!(
        metadata["sums_url"],
        "https://downloads.example.test/renamed/repository/releases/v1.2.3/SHA256SUMS"
    );

    let output = Command::new("bash")
        .arg(&metadata_script)
        .arg(&manifest_path)
        .arg("v1.2.3")
        .output()
        .expect("run Scoop version validation");
    assert!(
        !output.status.success(),
        "metadata materializer must independently validate the release version"
    );

    for invalid_template in [
        "",
        "https://downloads.example.test/releases/v$version/\nzeroclaw.zip",
        "https://downloads.example.test/releases/latest/zeroclaw.zip",
    ] {
        let invalid_manifest = serde_json::json!({
            "autoupdate": {
                "architecture": {
                    "64bit": {"url": invalid_template}
                }
            }
        });
        fs::write(
            &manifest_path,
            serde_json::to_vec(&invalid_manifest).expect("serialize invalid Scoop manifest"),
        )
        .expect("write invalid Scoop manifest");
        let output = Command::new("bash")
            .arg(&metadata_script)
            .arg(&manifest_path)
            .arg("1.2.3")
            .output()
            .expect("run Scoop metadata validation");
        assert!(
            !output.status.success(),
            "invalid canonical template must fail closed: {invalid_template:?}"
        );
    }
}
