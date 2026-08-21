//! Ratcheting parity tests between the canonical registry in `zeroclaw_dist`
//! and every repository surface that names a published distribution target.
//!
//! Each test reads a consuming source as text, relative to
//! `CARGO_MANIFEST_DIR`, and fails when either side drifts. A target change
//! must therefore land in the registry and its consumers together. Known
//! divergences are listed exactly, per triple with a reason, so a new
//! divergence can never slip through a wildcard.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use zeroclaw_dist::{DIST_TARGETS, PlatformFamily, ReleaseTier, find_target, required_targets};

/// Required release assets that are not per-target archives: the macOS
/// desktop bundle and the two SBOM documents (listed with the literal
/// `${TAG}` placeholder the workflow uses). Kept exact so a new non-target
/// asset must be classified here deliberately.
const KNOWN_NON_TARGET_REQUIRED_ASSETS: &[&str] = &[
    "ZeroClaw.dmg",
    "zeroclaw-${TAG}-sbom.spdx.json",
    "zeroclaw-${TAG}-sbom.cdx.json",
];

/// Registered targets `install.sh detect_target_triple` can never emit, each
/// with the reason it is exempt from host detection.
const KNOWN_INSTALL_UNDETECTABLE_TARGETS: &[(&str, &str)] = &[(
    "x86_64-pc-windows-msvc",
    "non-host: install.sh is the Unix bootstrap and never runs on Windows; \
         setup.bat selects this target",
)];

/// Triples `src/commands/update.rs target_triple_for` resolves that have no
/// published release archive. Reconciling each entry — registering the target
/// or making the updater fail with a clear unsupported-platform error — is
/// the responsibility of the follow-up updater PR, not this registry slice.
const KNOWN_UPDATE_EXTRA_TARGETS: &[(&str, &str)] = &[
    (
        "aarch64-pc-windows-msvc",
        "update.rs resolves Windows-on-ARM hosts to this triple, but no \
         release archive is published for it; the follow-up updater PR must \
         register the target or reject the platform explicitly",
    ),
    (
        "x86_64-pc-windows-gnu",
        "update.rs resolves windows-gnu toolchain builds to this triple, but \
         no release archive is published for it; the follow-up updater PR \
         must map windows-gnu hosts onto the canonical x86_64-pc-windows-msvc \
         archive or reject them explicitly",
    ),
];

/// Published targets `src/commands/update.rs target_triple_for` cannot emit.
///
/// The musl entries are more severe than ordinary unsupported targets: the
/// updater currently maps those hosts to the corresponding glibc triples
/// because its resolver has no libc input. The follow-up updater PR must make
/// libc part of resolution before it consumes this canonical registry.
const KNOWN_UPDATE_MISSING_TARGETS: &[(&str, &str)] = &[
    (
        "x86_64-unknown-linux-musl",
        "update.rs has no libc input and mis-resolves x86_64 musl hosts to the glibc target; the follow-up updater PR must detect libc",
    ),
    (
        "aarch64-unknown-linux-musl",
        "update.rs has no libc input and mis-resolves aarch64 musl hosts to the glibc target; the follow-up updater PR must detect libc",
    ),
    (
        "armv7-unknown-linux-gnueabihf",
        "update.rs receives the generic arm architecture and returns no target; the follow-up updater PR must distinguish supported ARM variants",
    ),
    (
        "arm-unknown-linux-gnueabihf",
        "update.rs receives the generic arm architecture and returns no target; the follow-up updater PR must distinguish supported ARM variants",
    ),
    (
        "aarch64-linux-android",
        "update.rs has no Android match arm and returns no target; the follow-up updater PR must support or explicitly reject the experimental target",
    ),
];

fn repo_file(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn registry_triples() -> BTreeSet<&'static str> {
    DIST_TARGETS.iter().map(|t| t.triple).collect()
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Extracts the lines of a top-level `name() {` shell function, up to its
/// closing `}` at column zero.
fn shell_function_body(script: &str, name: &str) -> String {
    let header = format!("{name}() {{");
    let mut body = String::new();
    let mut in_function = false;
    for line in script.lines() {
        if !in_function {
            if line.trim_end() == header {
                in_function = true;
            }
            continue;
        }
        if line == "}" {
            return body;
        }
        body.push_str(line);
        body.push('\n');
    }
    panic!("shell function {name} not found or unterminated");
}

/// Collects every `echo "..."` literal in a shell snippet.
fn echoed_literals(body: &str) -> Vec<String> {
    let mut literals = Vec::new();
    for line in body
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
    {
        let mut rest = line;
        while let Some(start) = rest.find("echo \"") {
            let after = &rest[start + "echo \"".len()..];
            let end = after
                .find('"')
                .unwrap_or_else(|| panic!("unterminated echo literal after: {after}"));
            literals.push(after[..end].to_string());
            rest = &after[end + 1..];
        }
    }
    literals
}

struct MatrixEntry {
    target: String,
    artifact: String,
    ext: String,
    experimental: bool,
}

/// Parses the single `matrix.include` block of the release workflow into its
/// entries, keyed by the fields this registry mirrors.
fn release_matrix_entries(workflow: &str) -> Vec<MatrixEntry> {
    let lines: Vec<&str> = workflow.lines().collect();
    let include_lines: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim() == "include:")
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        include_lines.len(),
        1,
        "expected exactly one matrix include block in the release workflow"
    );
    let include_index = include_lines[0];
    let include_indent = indent_of(lines[include_index]);

    let mut raw_entries: Vec<Vec<(String, String)>> = Vec::new();
    for line in &lines[include_index + 1..] {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if indent_of(line) <= include_indent {
            break;
        }
        let key_value = match trimmed.strip_prefix("- ") {
            Some(rest) => {
                raw_entries.push(Vec::new());
                rest
            }
            None => trimmed,
        };
        let (key, value) = key_value
            .split_once(':')
            .unwrap_or_else(|| panic!("matrix line without key: value form: {trimmed}"));
        raw_entries
            .last_mut()
            .expect("matrix key encountered before the first `- ` entry")
            .push((key.trim().to_string(), value.trim().to_string()));
    }
    assert!(
        !raw_entries.is_empty(),
        "release matrix include block is empty"
    );

    raw_entries
        .into_iter()
        .map(|fields| {
            let get = |name: &str| {
                fields
                    .iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.clone())
                    .unwrap_or_else(|| panic!("matrix entry missing `{name}`: {fields:?}"))
            };
            MatrixEntry {
                target: get("target"),
                artifact: get("artifact"),
                ext: get("ext"),
                experimental: fields
                    .iter()
                    .any(|(key, value)| key == "experimental" && value == "true"),
            }
        })
        .collect()
}

/// Collects the quoted entries of the workflow's `required_assets=( ... )` list.
fn required_asset_entries(workflow: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut in_list = false;
    for line in workflow.lines() {
        let trimmed = line.trim();
        if !in_list {
            if trimmed == "required_assets=(" {
                in_list = true;
            }
            continue;
        }
        if trimmed == ")" {
            assert!(!entries.is_empty(), "required_assets list is empty");
            return entries;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        let entry = trimmed
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or_else(|| panic!("required_assets entry is not a quoted literal: {trimmed}"));
        entries.push(entry.to_string());
    }
    panic!("required_assets list not found or unterminated in the release workflow");
}

/// Extracts the body of the top-level `fn target_triple_for(` in update.rs,
/// up to its closing `}` at column zero, and returns its `Some("...")`
/// literals.
fn update_resolver_outputs(source: &str) -> BTreeSet<String> {
    let mut body = String::new();
    let mut in_function = false;
    for line in source.lines() {
        if !in_function {
            if line.trim_start().starts_with("fn target_triple_for(") {
                in_function = true;
            }
            continue;
        }
        if line == "}" {
            in_function = false;
            break;
        }
        body.push_str(line);
        body.push('\n');
    }
    assert!(
        !in_function && !body.is_empty(),
        "fn target_triple_for not found or unterminated in update.rs"
    );

    let mut outputs = BTreeSet::new();
    let mut rest = body.as_str();
    while let Some(start) = rest.find("Some(\"") {
        let after = &rest[start + "Some(\"".len()..];
        let end = after
            .find('"')
            .unwrap_or_else(|| panic!("unterminated Some literal after: {after}"));
        outputs.insert(after[..end].to_string());
        rest = &after[end + 1..];
    }
    assert!(
        !outputs.is_empty(),
        "target_triple_for yielded no Some literals"
    );
    outputs
}

#[test]
fn release_workflow_matrix_matches_registry_in_both_directions() {
    let workflow = repo_file(".github/workflows/release-stable-manual.yml");
    let entries = release_matrix_entries(&workflow);

    let matrix_order: Vec<&str> = entries.iter().map(|entry| entry.target.as_str()).collect();
    let registry_order: Vec<&str> = DIST_TARGETS.iter().map(|target| target.triple).collect();
    assert_eq!(
        matrix_order, registry_order,
        "DIST_TARGETS must preserve release workflow matrix order"
    );

    let matrix_triples: BTreeSet<&str> = entries.iter().map(|e| e.target.as_str()).collect();
    assert_eq!(
        matrix_triples.len(),
        entries.len(),
        "duplicate target in the release matrix"
    );
    assert_eq!(
        matrix_triples,
        registry_triples(),
        "release workflow matrix targets and DIST_TARGETS must be identical"
    );

    for entry in &entries {
        let target = find_target(&entry.target)
            .unwrap_or_else(|| panic!("matrix target {} is not registered", entry.target));
        assert_eq!(
            entry.artifact, target.binary_name,
            "matrix artifact for {} disagrees with the registry binary name",
            entry.target
        );
        assert_eq!(
            entry.ext,
            target.archive.extension(),
            "matrix ext for {} disagrees with the registry archive kind",
            entry.target
        );
        assert_eq!(
            entry.experimental,
            target.tier == ReleaseTier::Experimental,
            "matrix experimental flag for {} disagrees with the registry tier",
            entry.target
        );
    }
}

#[test]
fn release_workflow_required_assets_match_required_targets() {
    let workflow = repo_file(".github/workflows/release-stable-manual.yml");
    let assets: BTreeSet<String> = required_asset_entries(&workflow).into_iter().collect();

    for known in KNOWN_NON_TARGET_REQUIRED_ASSETS {
        assert!(
            assets.contains(*known),
            "stale KNOWN_NON_TARGET_REQUIRED_ASSETS entry: {known} is no longer required"
        );
    }

    let target_assets: BTreeSet<&str> = assets
        .iter()
        .map(String::as_str)
        .filter(|asset| !KNOWN_NON_TARGET_REQUIRED_ASSETS.contains(asset))
        .collect();
    let expected: BTreeSet<String> = required_targets()
        .map(|t| format!("zeroclaw-{}.{}", t.triple, t.archive.extension()))
        .collect();
    let expected: BTreeSet<&str> = expected.iter().map(String::as_str).collect();
    assert_eq!(
        target_assets, expected,
        "workflow required_assets target archives and registry Required targets must be identical"
    );
}

#[test]
fn install_sh_detectable_triples_match_registry_with_exact_exceptions() {
    let install = repo_file("install.sh");

    let libc_variants: BTreeSet<String> =
        echoed_literals(&shell_function_body(&install, "detect_libc"))
            .into_iter()
            .collect();
    let expected_libcs: BTreeSet<String> = ["gnu", "musl"].map(String::from).into_iter().collect();
    assert_eq!(
        libc_variants, expected_libcs,
        "detect_libc outputs changed; revisit the ${{libc}} expansion below"
    );

    let mut detectable = BTreeSet::new();
    for literal in echoed_literals(&shell_function_body(&install, "detect_target_triple")) {
        if literal.is_empty() {
            continue;
        }
        if literal.contains("${libc}") {
            for libc in &libc_variants {
                detectable.insert(literal.replace("${libc}", libc));
            }
        } else {
            assert!(
                !literal.contains("${"),
                "unexpected parameterized triple literal in detect_target_triple: {literal}"
            );
            detectable.insert(literal);
        }
    }

    let registry = registry_triples();
    for triple in &detectable {
        assert!(
            registry.contains(triple.as_str()),
            "install.sh detects unregistered target {triple}"
        );
    }

    let detectable: BTreeSet<&str> = detectable.iter().map(String::as_str).collect();
    let undetectable: BTreeSet<&str> = registry.difference(&detectable).copied().collect();
    let known: BTreeSet<&str> = KNOWN_INSTALL_UNDETECTABLE_TARGETS
        .iter()
        .map(|(triple, _)| *triple)
        .collect();
    assert_eq!(
        known.len(),
        KNOWN_INSTALL_UNDETECTABLE_TARGETS.len(),
        "duplicate entry in KNOWN_INSTALL_UNDETECTABLE_TARGETS"
    );
    assert_eq!(
        undetectable, known,
        "registry targets not detectable by install.sh must exactly match the documented exceptions"
    );
}

#[test]
fn setup_bat_target_is_a_registered_canonical_windows_target() {
    let setup = repo_file("setup.bat");
    let assignments: Vec<&str> = setup
        .lines()
        .filter_map(|line| line.trim().strip_prefix("set \"TARGET="))
        .map(|rest| {
            rest.strip_suffix('"')
                .unwrap_or_else(|| panic!("unterminated TARGET assignment: {rest}"))
        })
        .collect();
    assert_eq!(
        assignments.len(),
        1,
        "expected exactly one TARGET assignment in setup.bat"
    );

    let target = find_target(assignments[0])
        .unwrap_or_else(|| panic!("setup.bat TARGET {} is not registered", assignments[0]));
    assert_eq!(target.family, PlatformFamily::Windows);
    assert_eq!(target.tier, ReleaseTier::Required);
}

#[test]
fn update_resolver_outputs_are_registered_or_exactly_known_divergences() {
    let update = repo_file("src/commands/update.rs");
    let outputs = update_resolver_outputs(&update);

    let registry = registry_triples();
    let diverging: BTreeSet<&str> = outputs
        .iter()
        .map(String::as_str)
        .filter(|triple| !registry.contains(triple))
        .collect();
    let known: BTreeSet<&str> = KNOWN_UPDATE_EXTRA_TARGETS
        .iter()
        .map(|(triple, _)| *triple)
        .collect();
    assert_eq!(
        known.len(),
        KNOWN_UPDATE_EXTRA_TARGETS.len(),
        "duplicate entry in KNOWN_UPDATE_EXTRA_TARGETS"
    );
    assert_eq!(
        diverging, known,
        "update.rs resolver outputs outside the registry must exactly match \
         KNOWN_UPDATE_EXTRA_TARGETS — register the target, fix the \
         resolver in the follow-up updater PR, or document the new divergence"
    );

    for (triple, reason) in KNOWN_UPDATE_EXTRA_TARGETS {
        assert!(
            find_target(triple).is_none(),
            "divergence entry {triple} is now registered; drop it from the list"
        );
        assert!(
            outputs.contains(*triple),
            "stale divergence entry {triple}: update.rs no longer resolves it"
        );
        assert!(
            reason.contains("follow-up updater PR"),
            "divergence entry {triple} must name the follow-up updater PR responsibility"
        );
    }

    let missing: BTreeSet<&str> = registry
        .difference(&outputs.iter().map(String::as_str).collect())
        .copied()
        .collect();
    let known_missing: BTreeSet<&str> = KNOWN_UPDATE_MISSING_TARGETS
        .iter()
        .map(|(triple, _)| *triple)
        .collect();
    assert_eq!(
        known_missing.len(),
        KNOWN_UPDATE_MISSING_TARGETS.len(),
        "duplicate entry in KNOWN_UPDATE_MISSING_TARGETS"
    );
    assert_eq!(
        missing, known_missing,
        "registered targets absent from update.rs must exactly match +         KNOWN_UPDATE_MISSING_TARGETS — fix the resolver in the follow-up +         updater PR or document the new divergence"
    );

    for (triple, reason) in KNOWN_UPDATE_MISSING_TARGETS {
        assert!(
            find_target(triple).is_some(),
            "missing-target entry {triple} is no longer registered; drop it from the list"
        );
        assert!(
            !outputs.contains(*triple),
            "missing-target entry {triple} is now resolved; drop it from the list"
        );
        assert!(
            reason.contains("follow-up updater PR"),
            "missing-target entry {triple} must name the follow-up updater PR responsibility"
        );
    }
}

#[test]
fn dist_target_exclusion_keys_are_registered_targets() {
    let manifest = repo_file("Cargo.toml");
    let mut keys = Vec::new();
    let mut in_table = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if !in_table {
            if trimmed == "[package.metadata.zeroclaw.dist_target_exclusions]" {
                in_table = true;
            }
            continue;
        }
        if trimmed.starts_with('[') {
            break;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (key, _) = trimmed
            .split_once('=')
            .unwrap_or_else(|| panic!("dist_target_exclusions line without `=`: {trimmed}"));
        keys.push(key.trim().trim_matches('"').to_string());
    }
    assert!(
        !keys.is_empty(),
        "dist_target_exclusions table not found or empty in root Cargo.toml"
    );
    for key in &keys {
        assert!(
            find_target(key).is_some(),
            "dist_target_exclusions key {key} is not a registered distribution target"
        );
    }
}
