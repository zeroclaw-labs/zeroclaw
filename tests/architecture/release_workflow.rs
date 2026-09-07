//! Architecture gates for release-workflow artifact identity.
//!
//! The macOS desktop job must notarize, staple, validate, and upload the same
//! DMG. Discovering the file independently in multiple steps can notarize one
//! image while publishing another.

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

fn bash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Resolve the interpreter that runs the POSIX release scripts below.
///
/// `Command::new("bash")` is not safe on Windows: `CreateProcess` searches the
/// system directory before `PATH`, and Windows ships a WSL launcher stub at
/// `%SystemRoot%\System32\bash.exe`. The GitHub Windows images carry that stub
/// without a WSL distribution, so it exits non-zero without running the script
/// and without writing anything to stderr — every script-driven gate here then
/// fails with an empty diagnostic. Resolve Git for Windows' Bash explicitly
/// (the same interpreter the workflow's own `shell: bash` steps use) and only
/// fall back to bare name resolution when no real Bash can be located.
fn bash_command() -> Command {
    Command::new(bash_program())
}

#[cfg(not(windows))]
fn bash_program() -> PathBuf {
    PathBuf::from("bash")
}

#[cfg(windows)]
fn bash_program() -> PathBuf {
    let git_bash = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|root| PathBuf::from(root).join("Git").join("bin").join("bash.exe"));

    let on_path = std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .filter(|directory| !is_windows_system_directory(directory))
                .map(|directory| directory.join("bash.exe"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    git_bash
        .chain(on_path)
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| PathBuf::from("bash"))
}

/// The WSL stub lives in the system directory; nothing else named `bash.exe`
/// is expected there, so excluding it is enough to reach a real interpreter.
#[cfg(windows)]
fn is_windows_system_directory(directory: &Path) -> bool {
    directory.file_name().is_some_and(|name| {
        let name = name.to_string_lossy().to_ascii_lowercase();
        name == "system32" || name == "syswow64"
    })
}

fn workflow(name: &str) -> String {
    let workflow_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".github/workflows")
        .join(name);
    fs::read_to_string(&workflow_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", workflow_path.display()))
}

/// Render everything a failed script run can tell us.
///
/// A misresolved or non-functional interpreter fails with an empty stderr, so
/// an assertion that reports stderr alone leaves nothing to diagnose. Include
/// the exit status and stdout as well.
fn command_diagnostics(output: &std::process::Output) -> String {
    format!(
        "status={:?} stdout={:?} stderr={:?}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_command_failure(
    output: &std::process::Output,
    expected_code: i32,
    expected_stderr: &str,
    context: &str,
) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(expected_code),
        "{context}: {stderr}"
    );
    assert!(
        stderr.contains(expected_stderr),
        "{context} must report {expected_stderr:?}, got: {stderr}"
    );
}

fn yaml_block<'a>(document: &'a str, header: &str) -> &'a str {
    let header_indent = header.len() - header.trim_start().len();
    let start = document
        .match_indices(header)
        .find_map(|(offset, _)| {
            (offset == 0 || document.as_bytes().get(offset - 1) == Some(&b'\n')).then_some(offset)
        })
        .unwrap_or_else(|| panic!("workflow is missing YAML block: {header}"));
    let remainder = &document[start + header.len()..];
    let end = remainder
        .split_inclusive('\n')
        .scan(0, |offset, line| {
            let line_start = *offset;
            *offset += line.len();
            Some((line_start, line))
        })
        .find_map(|(offset, line)| {
            let trimmed = line.trim();
            let indent = line.len() - line.trim_start().len();
            (!trimmed.is_empty() && indent <= header_indent).then_some(offset)
        })
        .unwrap_or(remainder.len());
    &remainder[..end]
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
    let publish_job = aur
        .split_once("  publish-aur:\n")
        .map(|(_, job)| job)
        .expect("AUR publisher must define the publish-aur job");
    assert!(
        publish_job
            .lines()
            .take(5)
            .any(|line| line == "    timeout-minutes: 20"),
        "the publish-aur job must have a bounded job timeout"
    );
    assert!(
        !aur.contains("ssh -T -o"),
        "AUR clone/push is the authoritative authentication check"
    );
    for required in [
        "group: aur-publish-${{ github.repository }}-${{ inputs.dry_run }}",
        "ref: refs/tags/${{ inputs.release_tag }}\n          path: release-source",
        "release-source/dist/aur/PKGBUILD",
        "release-source/dist/aur/.SRCINFO",
        "if: inputs.dry_run == false\n        timeout-minutes: 12",
        "\"${guard_command[@]}\" || return $?",
        "case \"$attempt_status\" in",
        "unexpected status ${attempt_status}",
        "Generated PKGBUILD is not valid Bash syntax",
        "tarball_url=\"https://github.com/zeroclaw-labs/zeroclaw/archive/refs/tags/${RELEASE_TAG}.tar.gz\"",
        "require_exact_line \"$PKGBUILD_FILE\" \"$expected_pkgbuild_source\"",
        "require_exact_line \"$SRCINFO_FILE\" \"$expected_srcinfo_source\"",
        "package metadata is missing, malformed, or inconsistent",
        "stopped to prevent a downgrade",
        "package files changed without a version-tuple change",
        "Release metadata is pinned to the immutable tag",
        "corrected source change must ship under a new release tag",
    ] {
        assert!(
            aur.contains(required),
            "AUR publisher is missing release-safety invariant: {required}"
        );
    }

    let workflow_call_inputs = aur
        .split_once("  workflow_call:\n")
        .and_then(|(_, remainder)| remainder.split_once("  workflow_dispatch:\n"))
        .map(|(block, _)| block)
        .expect("AUR publisher must define workflow_call before workflow_dispatch");
    assert!(
        !workflow_call_inputs.contains("allow_downgrade"),
        "automated reusable callers must not be able to authorize an AUR downgrade"
    );
    let manual_inputs = aur
        .split_once("  workflow_dispatch:\n")
        .and_then(|(_, remainder)| remainder.split_once("\nconcurrency:\n"))
        .map(|(block, _)| block)
        .expect("AUR publisher must define manual dispatch inputs");
    assert!(
        manual_inputs.contains("allow_downgrade:"),
        "manual recovery must expose an explicit downgrade override"
    );
    let downgrade_input = manual_inputs
        .split_once("allow_downgrade:")
        .map(|(_, block)| block)
        .expect("manual dispatch must expose allow_downgrade");
    assert!(
        downgrade_input.contains("default: false"),
        "manual downgrade authorization must default to false"
    );

    let guard_call = "scripts/release/aur_version_guard.sh";
    assert_eq!(
        aur.matches("scripts/release/aur_version_guard.sh").count(),
        2,
        "the AUR guard must validate generated metadata and each fresh clone"
    );

    let clone_position = aur
        .find("git clone --quiet ssh://aur@aur.archlinux.org/zeroclawlabs.git")
        .expect("AUR publisher must clone the authoritative package state");
    let guard_position = aur
        .rfind(guard_call)
        .expect("AUR publisher must enforce monotonic versions");
    let overwrite_position = aur
        .find("cp \"$PKGBUILD_FILE\" \"$work_dir/PKGBUILD\"")
        .expect("AUR publisher must update PKGBUILD");
    assert!(
        clone_position < guard_position && guard_position < overwrite_position,
        "the AUR monotonic guard must inspect each fresh clone before package metadata is overwritten"
    );

    let input_validation_position = aur
        .find("      - name: Validate release tag input\n")
        .expect("release tag input must be validated");
    let release_checkout_position = aur
        .find("      - name: Check out release package metadata\n")
        .expect("release package metadata must use an isolated checkout");
    assert!(
        input_validation_position < release_checkout_position,
        "release_tag must be validated before it is used as a checkout ref"
    );

    let generated_validation = aur
        .split_once("      - name: Validate generated AUR metadata\n")
        .and_then(|(_, remainder)| remainder.split_once("      - name: Push to AUR\n"))
        .map(|(step, _)| step)
        .expect("generated AUR metadata must be validated before the push step");
    assert!(
        generated_validation.contains(guard_call)
            && generated_validation.contains("\"$SRCINFO_FILE\" \"$SRCINFO_FILE\"")
            && generated_validation.contains("\"$PKGBUILD_FILE\" \"$PKGBUILD_FILE\""),
        "dry-run must exercise target-side metadata validation without an AUR clone"
    );

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source_srcinfo = root.join("dist/aur/.SRCINFO");
    let source_pkgbuild = root.join("dist/aur/PKGBUILD");
    let source_guard = bash_command()
        .arg(bash_path(
            &root.join("scripts/release/aur_version_guard.sh"),
        ))
        .arg(bash_path(&source_srcinfo))
        .arg(bash_path(&source_srcinfo))
        .arg(bash_path(&source_pkgbuild))
        .arg(bash_path(&source_pkgbuild))
        .output()
        .expect("validate checked-in AUR package metadata");
    assert!(
        source_guard.status.success(),
        "checked-in PKGBUILD and .SRCINFO version tuples must agree: {}",
        command_diagnostics(&source_guard)
    );

    let freshness = workflow("aur-freshness-check.yml");
    assert!(
        freshness.contains(
            "aur_epoch_pkgver=\"${aur_full%%-*}\"\n          aur_version=\"${aur_epoch_pkgver#*:}\""
        ),
        "AUR freshness must remove pkgrel and epoch before comparing pkgver to the release"
    );
    assert!(
        freshness.contains("sort -V | tail -n 1")
            && freshness.contains("AUR is newer than the release")
            && freshness.contains("source_epoch=\"$(git show")
            && freshness.contains("git show \"${tag}:dist/aur/.SRCINFO\"")
            && freshness.contains("\"$aur_epoch\" != \"$source_epoch\"")
            && freshness.contains("Do not use allow_downgrade across epochs")
            && freshness.contains("cut a new release tag")
            && freshness.contains("this check remains red until that tag is published"),
        "freshness must compare the published epoch and scope downgrade recovery advice"
    );
}

#[test]
fn crates_io_publisher_is_preflighted_gated_and_resumable() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let release = workflow("release-stable-manual.yml");
    let publisher = workflow("pub-crates.yml");
    let script_path = root.join("scripts/release/publish-crates.sh");
    let script = fs::read_to_string(&script_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", script_path.display()));

    for required in [
        "uses: ./.github/workflows/pub-crates.yml",
        "release_tag: ${{ needs.validate.outputs.tag }}",
        "release_sha: ${{ github.sha }}",
    ] {
        assert!(
            release.contains(required),
            "stable release is missing crates.io wiring: {required}"
        );
    }
    let crates_call = yaml_block(&release, "  crates:\n");
    assert!(
        crates_call
            .contains("secrets:\n      CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}"),
        "the reusable crates.io workflow must receive the existing repository secret explicitly"
    );
    assert!(
        !crates_call.contains("secrets: inherit"),
        "the crates.io caller must pass only the named registry secret"
    );

    let workflow_call = yaml_block(&publisher, "  workflow_call:\n");
    assert!(
        workflow_call.contains(
            "secrets:\n      CARGO_REGISTRY_TOKEN:\n        description: \"Repository-scoped crates.io token; referenced only by the protected publish job\"\n        required: true"
        ),
        "the reusable publisher must declare the explicitly mapped registry secret"
    );

    let preflight = yaml_block(&publisher, "  preflight:\n");
    let publish = yaml_block(&publisher, "  publish:\n");
    for required in [
        "ref: ${{ inputs.release_tag }}",
        "release_sha",
        "cargo test --locked --test architecture publish_contract",
        "CARGO_TARGET_DIR: ${{ runner.temp }}/crates-io-package-target",
        "./scripts/release/publish-crates.sh",
    ] {
        assert!(
            preflight.contains(required),
            "crates.io preflight is missing invariant: {required}"
        );
    }
    assert!(
        !preflight.contains("CARGO_REGISTRY_TOKEN"),
        "the reversible preflight must never reference the registry token"
    );
    for required in [
        "environment:\n      name: crates-io",
        "ref: ${{ needs.preflight.outputs.sha }}",
        "CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}",
        "./scripts/release/publish-crates.sh --execute",
    ] {
        assert!(
            publish.contains(required),
            "crates.io publish job is missing invariant: {required}"
        );
    }

    for required in [
        "EXECUTE=0",
        "--execute) EXECUTE=1",
        "cargo metadata --format-version 1 --no-deps",
        "git diff --quiet",
        "git ls-files --others --exclude-standard",
        "web/dist/index.html",
        "cargo publish --dry-run --locked --allow-dirty",
        "--locked --no-verify --allow-dirty",
        "Topological order over the publishable set",
        "wait_for_registry_version",
        "will skip what already landed",
    ] {
        assert!(
            script.contains(required),
            "publish-crates.sh is missing safety contract: {required}"
        );
    }
}

#[test]
fn aur_publisher_rejects_stale_release_downgrades() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let guard_script = root.join("scripts/release/aur_version_guard.sh");
    let temp = tempfile::tempdir().expect("create temporary AUR package directory");
    let target_srcinfo = temp.path().join("target.SRCINFO");
    let current_srcinfo = temp.path().join("current.SRCINFO");
    let target_pkgbuild = temp.path().join("target.PKGBUILD");
    let current_pkgbuild = temp.path().join("current.PKGBUILD");

    let srcinfo = |epoch: Option<u32>, version: &str, release: &str| {
        let epoch = epoch.map_or_else(String::new, |value| format!("epoch = {value}\n"));
        format!(
            "pkgbase = zeroclawlabs\n{epoch}pkgver = {version}\npkgrel = {release}\npkgname = zeroclawlabs\n"
        )
    };
    let pkgbuild = |epoch: Option<u32>, version: &str, release: &str| {
        let epoch = epoch.map_or_else(String::new, |value| format!("epoch={value}\n"));
        format!("pkgname=zeroclawlabs\n{epoch}pkgver={version}\npkgrel={release}\n")
    };

    let run_guard = |target_metadata: &str,
                     current_metadata: &str,
                     target_build: &str,
                     current_build: &str,
                     allow_downgrade: bool| {
        fs::write(&target_srcinfo, target_metadata).expect("write target AUR .SRCINFO");
        fs::write(&current_srcinfo, current_metadata).expect("write current AUR .SRCINFO");
        fs::write(&target_pkgbuild, target_build).expect("write target AUR PKGBUILD");
        fs::write(&current_pkgbuild, current_build).expect("write current AUR PKGBUILD");
        let mut command = bash_command();
        command.arg(bash_path(&guard_script));
        if allow_downgrade {
            command.arg("--allow-downgrade");
        }
        command
            .arg(bash_path(&target_srcinfo))
            .arg(bash_path(&current_srcinfo))
            .arg(bash_path(&target_pkgbuild))
            .arg(bash_path(&current_pkgbuild))
            .output()
            .expect("run AUR monotonic package guard")
    };

    let same_build = pkgbuild(None, "1.2.3", "1");
    let equal = srcinfo(None, "1.2.3", "1");
    let output = run_guard(&equal, &equal, &same_build, &same_build, false);
    assert!(
        output.status.success(),
        "an unchanged package must be idempotent: {}",
        command_diagnostics(&output)
    );

    for (target, current) in [("1.2.4", "1.2.3"), ("1.10.0", "1.9.9")] {
        let output = run_guard(
            &srcinfo(None, target, "1"),
            &srcinfo(None, current, "1"),
            &pkgbuild(None, target, "1"),
            &pkgbuild(None, current, "1"),
            false,
        );
        assert!(
            output.status.success(),
            "target {target} should be allowed over {current}: {}",
            command_diagnostics(&output)
        );
    }

    let older = srcinfo(None, "1.9.9", "1");
    let newer = srcinfo(None, "1.10.0", "1");
    let older_build = pkgbuild(None, "1.9.9", "1");
    let newer_build = pkgbuild(None, "1.10.0", "1");
    let output = run_guard(&older, &newer, &older_build, &newer_build, false);
    assert_eq!(
        output.status.code(),
        Some(3),
        "an older workflow must return the dedicated downgrade status"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Refusing AUR downgrade"),
        "downgrade rejection must explain why publishing stopped"
    );

    let output = run_guard(&older, &newer, &older_build, &newer_build, true);
    assert!(
        output.status.success()
            && String::from_utf8_lossy(&output.stderr).contains("Manual AUR downgrade override"),
        "an explicit manual override must permit a deliberate rollback"
    );

    let output = run_guard(
        &srcinfo(None, "1.2.3", "1"),
        &srcinfo(None, "1.2.3", "2"),
        &pkgbuild(None, "1.2.3", "1"),
        &pkgbuild(None, "1.2.3", "2"),
        false,
    );
    assert_eq!(
        output.status.code(),
        Some(3),
        "pkgrel must participate in monotonic package ordering"
    );

    let output = run_guard(
        &srcinfo(None, "2.0.0", "1"),
        &srcinfo(Some(1), "1.0.0", "1"),
        &pkgbuild(None, "2.0.0", "1"),
        &pkgbuild(Some(1), "1.0.0", "1"),
        false,
    );
    assert_eq!(
        output.status.code(),
        Some(3),
        "epoch must take precedence over pkgver"
    );

    let output = run_guard(
        &srcinfo(None, "2.0.0", "1"),
        &srcinfo(Some(1), "1.0.0", "1"),
        &pkgbuild(None, "2.0.0", "1"),
        &pkgbuild(Some(1), "1.0.0", "1"),
        true,
    );
    assert_eq!(
        output.status.code(),
        Some(3),
        "manual downgrade authorization must not cross an epoch boundary"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Refusing manual AUR downgrade override across an epoch boundary"),
        "cross-epoch rejection must give actionable recovery guidance"
    );

    let changed_build = format!("{same_build}# changed metadata\n");
    let output = run_guard(&equal, &equal, &changed_build, &same_build, false);
    assert_eq!(
        output.status.code(),
        Some(4),
        "different package files must not reuse an existing version tuple"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "a changed package must ship under a new version tuple from a new release tag"
        ),
        "same-version drift guidance must account for immutable release metadata"
    );

    let output = run_guard(&equal, &equal, &changed_build, &same_build, true);
    assert_eq!(
        output.status.code(),
        Some(4),
        "manual downgrade authorization must not permit same-version rewrites"
    );

    let malformed = srcinfo(None, "not-a-version", "1");
    let output = run_guard(&equal, &malformed, &same_build, &same_build, false);
    assert_command_failure(
        &output,
        2,
        "Current AUR pkgver is not numeric",
        "unparseable current AUR state must return a hard validation failure",
    );
    let output = run_guard(&equal, &malformed, &same_build, &same_build, true);
    assert_command_failure(
        &output,
        2,
        "Current AUR pkgver is not numeric",
        "manual downgrade authorization must not permit malformed AUR state",
    );

    let extra_equals = equal.replace("pkgver = 1.2.3", "pkgver = 1.2.3 = junk");
    let output = run_guard(&equal, &extra_equals, &same_build, &same_build, false);
    assert_command_failure(
        &output,
        2,
        "Current AUR pkgver is not numeric",
        "SRCINFO values with trailing equals data must not be truncated",
    );

    let malformed_build = same_build.replace("pkgver=1.2.3", "pkgver=1.2.3=junk");
    let output = run_guard(&equal, &equal, &same_build, &malformed_build, false);
    assert_command_failure(
        &output,
        2,
        "Current PKGBUILD pkgver is not numeric",
        "PKGBUILD values with trailing equals data must not be truncated",
    );

    let duplicate = format!("{equal}pkgver = 9.9.9\n");
    let output = run_guard(&equal, &duplicate, &same_build, &same_build, false);
    assert_command_failure(
        &output,
        2,
        "Expected exactly one pkgver in Current .SRCINFO; found 2",
        "multiple pkgver fields must fail closed",
    );

    let mismatched_build = pkgbuild(None, "1.2.3", "2");
    let output = run_guard(&equal, &equal, &mismatched_build, &same_build, false);
    assert_command_failure(
        &output,
        2,
        "Generated AUR .SRCINFO and PKGBUILD disagree",
        "generated PKGBUILD and .SRCINFO version tuples must agree",
    );

    fs::write(&target_srcinfo, &equal).expect("restore target AUR .SRCINFO");
    fs::write(&target_pkgbuild, &same_build).expect("restore target AUR PKGBUILD");
    fs::remove_file(&current_srcinfo).expect("remove current AUR .SRCINFO");
    fs::remove_file(&current_pkgbuild).expect("remove current AUR PKGBUILD");
    let output = bash_command()
        .arg(bash_path(&guard_script))
        .arg(bash_path(&target_srcinfo))
        .arg(bash_path(&current_srcinfo))
        .arg(bash_path(&target_pkgbuild))
        .arg(bash_path(&current_pkgbuild))
        .output()
        .expect("run AUR guard with missing current metadata");
    assert_command_failure(
        &output,
        2,
        "cloned AUR repository is unexpectedly empty",
        "an empty clone must not implicitly authorize a first publish",
    );

    fs::write(&current_srcinfo, &equal).expect("restore only current AUR .SRCINFO");
    let output = bash_command()
        .arg(bash_path(&guard_script))
        .arg(bash_path(&target_srcinfo))
        .arg(bash_path(&current_srcinfo))
        .arg(bash_path(&target_pkgbuild))
        .arg(bash_path(&current_pkgbuild))
        .output()
        .expect("run AUR guard with partial current metadata");
    assert_command_failure(
        &output,
        2,
        "cloned AUR repository is partially populated",
        "a partially populated cloned package must fail closed",
    );
}

#[test]
fn scoop_credential_canary_fails_closed_without_weakening_generic_dry_runs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let gate = root.join("scripts/release/scoop_credential_gate.sh");

    let run_gate = |dry_run: &str,
                    credential_canary: &str,
                    bucket_repo: Option<&str>,
                    bucket_token: Option<&str>| {
        let mut command = bash_command();
        command
            .arg(bash_path(&gate))
            .env("DRY_RUN", dry_run)
            .env("CREDENTIAL_CANARY", credential_canary)
            .env_remove("SCOOP_BUCKET_REPO")
            .env_remove("GH_TOKEN");
        if let Some(repo) = bucket_repo {
            command.env("SCOOP_BUCKET_REPO", repo);
        }
        if let Some(token) = bucket_token {
            command.env("GH_TOKEN", token);
        }
        command.output().expect("run Scoop credential gate")
    };

    let generic_dry_run = run_gate("true", "false", None, None);
    assert!(
        generic_dry_run.status.success(),
        "a generic dry run may omit bucket credentials: {}",
        command_diagnostics(&generic_dry_run)
    );
    assert_eq!(generic_dry_run.stdout, b"skip\n");

    for (repo, token, missing) in [
        (None, Some("test-token"), "repository"),
        (Some("example/scoop-bucket"), None, "token"),
    ] {
        let canary = run_gate("true", "true", repo, token);
        assert!(
            !canary.status.success(),
            "credential canary must fail when the {missing} is missing"
        );
    }

    let configured_canary = run_gate(
        "true",
        "true",
        Some("example/scoop-bucket"),
        Some("test-token"),
    );
    assert!(
        configured_canary.status.success(),
        "configured credential canary must reach the authorization probe: {}",
        command_diagnostics(&configured_canary)
    );
    assert_eq!(configured_canary.stdout, b"probe\n");

    for (repo, token, missing) in [
        (None, Some("test-token"), "repository"),
        (Some("example/scoop-bucket"), None, "token"),
    ] {
        let publish = run_gate("false", "false", repo, token);
        assert!(
            !publish.status.success(),
            "real publish must fail when the {missing} is missing"
        );
    }

    for (dry_run, credential_canary, variable) in [
        ("yes", "false", "DRY_RUN"),
        ("", "false", "DRY_RUN"),
        ("true", "yes", "CREDENTIAL_CANARY"),
        ("true", "", "CREDENTIAL_CANARY"),
    ] {
        let invalid = run_gate(
            dry_run,
            credential_canary,
            Some("example/scoop-bucket"),
            Some("test-token"),
        );
        assert!(
            !invalid.status.success(),
            "invalid {variable} value must fail closed"
        );
    }

    let canary_workflow = workflow("scoop-bucket-canary.yml");
    let canary_triggers = yaml_block(&canary_workflow, "on:\n");
    assert!(
        canary_triggers.contains("- cron: \"23 7 * * 1\""),
        "the Scoop canary must keep its weekly schedule trigger"
    );
    assert!(
        canary_triggers.contains("  workflow_dispatch:"),
        "the Scoop canary must stay manually dispatchable for credential rotation proof"
    );
    let canary_resolve_job = yaml_block(&canary_workflow, "  latest-release:\n");
    assert!(
        !canary_resolve_job.lines().any(|line| {
            line.starts_with("    if:") || line.starts_with("    continue-on-error:")
        }),
        "the Scoop canary tag resolver must run and fail closed on every scheduled invocation"
    );
    let canary_job = yaml_block(&canary_workflow, "  rehearse:\n");
    assert!(
        !canary_job.lines().any(|line| {
            line.starts_with("    if:") || line.starts_with("    continue-on-error:")
        }),
        "the Scoop canary rehearsal must run and fail closed on every scheduled invocation"
    );
    for required in [
        "uses: ./.github/workflows/pub-scoop.yml",
        "dry_run: true",
        "credential_canary: true",
        "SCOOP_BUCKET_TOKEN: ${{ secrets.SCOOP_BUCKET_TOKEN }}",
    ] {
        assert!(
            canary_job.contains(required),
            "Scoop canary is missing fail-closed invariant: {required}"
        );
    }
    assert!(
        !canary_job.contains("secrets: inherit"),
        "Scoop canary must receive only the named bucket token"
    );
    let canary_secrets = yaml_block(canary_job, "    secrets:\n");
    let canary_secret_names = canary_secrets
        .lines()
        .filter(|line| line.starts_with("      ") && !line.starts_with("       "))
        .map(str::trim)
        .collect::<Vec<_>>();
    assert_eq!(
        canary_secret_names,
        ["SCOOP_BUCKET_TOKEN: ${{ secrets.SCOOP_BUCKET_TOKEN }}"],
        "Scoop canary must map exactly the one secret its callee declares"
    );

    let release_workflow = workflow("release-stable-manual.yml");
    let release_scoop_job = yaml_block(&release_workflow, "  scoop:\n");
    for required in [
        "uses: ./.github/workflows/pub-scoop.yml",
        "dry_run: false",
        "SCOOP_BUCKET_TOKEN: ${{ secrets.SCOOP_BUCKET_TOKEN }}",
    ] {
        assert!(
            release_scoop_job.contains(required),
            "real Scoop publisher caller is missing invariant: {required}"
        );
    }
    assert!(
        !release_scoop_job.contains("secrets: inherit"),
        "real Scoop publisher must receive only the named bucket token"
    );
    let release_scoop_secrets = yaml_block(release_scoop_job, "    secrets:\n");
    let release_scoop_secret_names = release_scoop_secrets
        .lines()
        .filter(|line| line.starts_with("      ") && !line.starts_with("       "))
        .map(str::trim)
        .collect::<Vec<_>>();
    assert_eq!(
        release_scoop_secret_names,
        ["SCOOP_BUCKET_TOKEN: ${{ secrets.SCOOP_BUCKET_TOKEN }}"],
        "real Scoop caller must map exactly the one secret its callee declares"
    );
    assert!(
        !release_scoop_job
            .lines()
            .any(|line| line.starts_with("    continue-on-error:")),
        "real Scoop publisher failures must stay fatal"
    );
    let release_scoop_conditions = release_scoop_job
        .lines()
        .filter(|line| line.starts_with("    if:"))
        .collect::<Vec<_>>();
    assert_eq!(
        release_scoop_conditions,
        ["    if: ${{ !cancelled() && needs.publish.result == 'success' }}"],
        "real Scoop publisher must stay gated only on a successful publish"
    );

    let publisher_workflow = workflow("pub-scoop.yml");
    let publisher_triggers = yaml_block(&publisher_workflow, "on:\n");
    let publisher_dispatch = yaml_block(publisher_triggers, "  workflow_dispatch:\n");
    assert!(
        yaml_block(publisher_dispatch, "      dry_run:\n").contains("default: true"),
        "manual Scoop publisher dispatch must stay non-destructive by default"
    );
    let workflow_call = yaml_block(&publisher_workflow, "  workflow_call:\n");
    let workflow_call_secrets = yaml_block(workflow_call, "    secrets:\n");
    let scoop_token = yaml_block(workflow_call_secrets, "      SCOOP_BUCKET_TOKEN:\n");
    assert!(
        scoop_token.contains("required: true"),
        "reusable Scoop publisher must require its declared bucket token"
    );
    let declared_secrets = workflow_call_secrets
        .lines()
        .filter(|line| line.starts_with("      ") && !line.starts_with("       "))
        .collect::<Vec<_>>();
    assert_eq!(
        declared_secrets,
        ["      SCOOP_BUCKET_TOKEN:"],
        "reusable Scoop publisher must declare exactly one secret"
    );

    let publisher_job = yaml_block(&publisher_workflow, "  publish-scoop:\n");
    assert!(
        !publisher_job.lines().any(|line| {
            line.starts_with("    if:") || line.starts_with("    continue-on-error:")
        }),
        "the Scoop publisher job must run and fail closed on every invocation, including canary dry runs"
    );
    let push_step = yaml_block(publisher_job, "      - name: Push to Scoop bucket\n");
    assert_eq!(
        push_step
            .lines()
            .filter(|line| line.starts_with("        if:"))
            .collect::<Vec<_>>(),
        ["        if: inputs.dry_run == false"],
        "the bucket write must stay gated on a non-dry-run so canary rehearsals never push"
    );
    assert!(
        !push_step
            .lines()
            .any(|line| line.starts_with("        continue-on-error:")),
        "real bucket write failures must stay fatal"
    );
    let publisher_env = yaml_block(publisher_job, "    env:\n");
    let canary_env = "CREDENTIAL_CANARY: ${{ inputs.credential_canary }}";
    assert_eq!(
        publisher_env.matches(canary_env).count(),
        1,
        "publisher job-level env must map credential_canary into the tested gate exactly once"
    );
    assert_eq!(
        publisher_workflow.matches("CREDENTIAL_CANARY").count(),
        1,
        "credential_canary must have exactly one uppercase env binding, at publisher job scope"
    );
    let dry_run_env = "DRY_RUN: ${{ inputs.dry_run }}";
    assert_eq!(
        publisher_env.matches(dry_run_env).count(),
        1,
        "publisher job-level env must map dry_run into the tested gate exactly once"
    );
    assert_eq!(
        publisher_workflow.matches("DRY_RUN").count(),
        1,
        "dry_run must have exactly one uppercase env binding, at publisher job scope"
    );

    let validate_step = yaml_block(
        publisher_job,
        "      - name: Validate Scoop publish configuration\n",
    );
    assert!(
        !validate_step.lines().any(|line| {
            line.starts_with("        if:") || line.starts_with("        continue-on-error:")
        }),
        "the Scoop credential gate step must run and fail closed on every invocation, including canary dry runs"
    );
    assert!(
        validate_step.contains("gate_result=\"$(bash scripts/release/scoop_credential_gate.sh)\""),
        "Scoop publisher must enforce the tested credential gate"
    );
    assert!(
        validate_step.contains("push --dry-run origin HEAD"),
        "the authorization probe must live in the unconditional credential gate step"
    );
    assert_eq!(
        publisher_workflow
            .matches("push --dry-run origin HEAD")
            .count(),
        1,
        "Scoop publisher must keep exactly one authoritative authorization probe"
    );
}

#[test]
fn scoop_metadata_template_is_not_evaluated() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let metadata_script = root.join("scripts/release/scoop_metadata.sh");
    let script = fs::read_to_string(&metadata_script)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", metadata_script.display()));
    assert!(
        !script.contains("eval "),
        "canonical Scoop URL templates must never be evaluated as shell code"
    );
}

#[test]
#[cfg(unix)]
fn scoop_publisher_metadata_follows_canonical_url_template() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let metadata_script = root.join("scripts/release/scoop_metadata.sh");
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

    let output = bash_command()
        .arg(bash_path(&metadata_script))
        .arg(bash_path(&manifest_path))
        .arg("1.2.3")
        .output()
        .expect("run Scoop metadata materializer");
    assert!(
        output.status.success(),
        "Scoop metadata materializer failed: {}",
        command_diagnostics(&output)
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

    let output = bash_command()
        .arg(bash_path(&metadata_script))
        .arg(bash_path(&manifest_path))
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
        let output = bash_command()
            .arg(bash_path(&metadata_script))
            .arg(bash_path(&manifest_path))
            .arg("1.2.3")
            .output()
            .expect("run Scoop metadata validation");
        assert!(
            !output.status.success(),
            "invalid canonical template must fail closed: {invalid_template:?}"
        );
    }
}
