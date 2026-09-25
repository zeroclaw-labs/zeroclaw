use std::collections::BTreeSet;

fn parse_manifest(source: &str, name: &str) -> toml::Value {
    toml::from_str(source).unwrap_or_else(|error| panic!("{name} must be valid TOML: {error}"))
}

fn feature_table<'a>(manifest: &'a toml::Value, name: &str) -> &'a toml::Table {
    manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .unwrap_or_else(|| panic!("{name} must define a [features] table"))
}

fn feature_values<'a>(features: &'a toml::Table, feature: &str) -> Vec<&'a str> {
    features
        .get(feature)
        .and_then(toml::Value::as_array)
        .unwrap_or_else(|| panic!("manifest must define feature {feature}"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("feature {feature} must contain only strings"))
        })
        .collect()
}

fn assert_feature_contains(label: &str, values: &[&str], expected: &str) {
    assert!(
        values.contains(&expected),
        "{label} must include {expected}; actual feature values: {values:?}"
    );
}

fn root_feature_reachable<'a>(features: &'a toml::Table, seeds: &[&'a str]) -> BTreeSet<&'a str> {
    let mut reachable = BTreeSet::new();
    let mut visited_root_features = BTreeSet::new();
    let mut pending = seeds.to_vec();

    while let Some(feature) = pending.pop() {
        if !visited_root_features.insert(feature) {
            continue;
        }
        reachable.insert(feature);

        for reference in feature_values(features, feature) {
            reachable.insert(reference);
            if !reference.starts_with("dep:")
                && !reference.contains('/')
                && features.contains_key(reference)
            {
                pending.push(reference);
            }
        }
    }

    reachable
}

fn distribution_inputs(root: &toml::Value) -> Vec<&str> {
    let extra_features = root
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("metadata"))
        .and_then(toml::Value::as_table)
        .and_then(|metadata| metadata.get("zeroclaw"))
        .and_then(toml::Value::as_table)
        .and_then(|zeroclaw| zeroclaw.get("dist_extra_features"))
        .and_then(toml::Value::as_array)
        .expect("root manifest must define dist_extra_features");

    let mut inputs = vec!["default"];
    inputs.extend(extra_features.iter().map(|feature| {
        feature
            .as_str()
            .expect("dist_extra_features must contain only strings")
    }));
    inputs
}

fn dependency_feature(reference: &str) -> Option<(&str, &str, bool)> {
    let (dependency, feature) = reference.split_once('/')?;
    let (dependency, weak) = match dependency.strip_suffix('?') {
        Some(dependency) => (dependency, true),
        None => (dependency, false),
    };
    Some((dependency, feature, weak))
}

fn active_dependencies<'a>(reachable: &BTreeSet<&'a str>) -> BTreeSet<&'a str> {
    reachable
        .iter()
        .copied()
        .filter_map(|reference| {
            if let Some(dependency) = reference.strip_prefix("dep:") {
                return Some(dependency);
            }
            dependency_feature(reference)
                .and_then(|(dependency, _, weak)| (!weak).then_some(dependency))
        })
        .collect()
}

fn probe_boundary_violations<'a>(reachable: &BTreeSet<&'a str>) -> Vec<&'a str> {
    let active_dependencies = active_dependencies(reachable);
    reachable
        .iter()
        .copied()
        .filter(|reference| {
            if matches!(*reference, "hardware" | "probe" | "dep:zeroclaw-hardware") {
                return true;
            }

            let Some((dependency, feature, weak)) = dependency_feature(reference) else {
                return false;
            };
            let dependency_feature_is_active = !weak || active_dependencies.contains(dependency);
            dependency_feature_is_active
                && ((dependency == "zeroclaw-hardware" && matches!(feature, "hardware" | "probe"))
                    || (dependency == "zeroclaw-tools" && feature == "probe"))
        })
        .collect()
}

fn assert_probe_boundary(profile: &str, reachable: &BTreeSet<&str>) {
    let violations = probe_boundary_violations(reachable);
    assert!(
        violations.is_empty(),
        "{profile} must not reach hardware or probe features; original offending reachable \
         values: {violations:?}; observed reachable values: {reachable:?}"
    );
}

#[test]
fn probe_boundary_applies_weak_dependency_feature_semantics() {
    let active_weak_edge = BTreeSet::from(["dep:zeroclaw-tools", "zeroclaw-tools?/probe"]);
    assert_eq!(
        probe_boundary_violations(&active_weak_edge),
        vec!["zeroclaw-tools?/probe"]
    );

    let inactive_weak_edge = BTreeSet::from(["zeroclaw-tools?/probe"]);
    assert!(probe_boundary_violations(&inactive_weak_edge).is_empty());

    let strong_edge = BTreeSet::from(["zeroclaw-tools/probe"]);
    assert_eq!(
        probe_boundary_violations(&strong_edge),
        vec!["zeroclaw-tools/probe"]
    );
}

#[test]
fn probe_feature_graph_preserves_forwarding_and_distribution_boundaries() {
    let root = parse_manifest(include_str!("../../Cargo.toml"), "root Cargo.toml");
    let hardware = parse_manifest(
        include_str!("../../crates/zeroclaw-hardware/Cargo.toml"),
        "zeroclaw-hardware Cargo.toml",
    );
    let tools = parse_manifest(
        include_str!("../../crates/zeroclaw-tools/Cargo.toml"),
        "zeroclaw-tools Cargo.toml",
    );
    let root_features = feature_table(&root, "root Cargo.toml");
    let hardware_features = feature_table(&hardware, "zeroclaw-hardware Cargo.toml");
    let tools_features = feature_table(&tools, "zeroclaw-tools Cargo.toml");

    let root_hardware = feature_values(root_features, "hardware");
    assert_feature_contains("root hardware", &root_hardware, "dep:zeroclaw-hardware");
    assert_feature_contains(
        "root hardware",
        &root_hardware,
        "zeroclaw-hardware/hardware",
    );

    let root_probe = feature_values(root_features, "probe");
    assert_feature_contains("root probe", &root_probe, "dep:zeroclaw-hardware");
    assert_feature_contains("root probe", &root_probe, "zeroclaw-hardware/probe");

    let ci_all_reachable = root_feature_reachable(root_features, &["ci-all"]);
    for expected in ["hardware", "probe"] {
        assert!(
            ci_all_reachable.contains(expected),
            "root ci-all must reach {expected}; observed reachable values: {ci_all_reachable:?}"
        );
    }

    let hardware_probe = feature_values(hardware_features, "probe");
    assert_feature_contains("zeroclaw-hardware probe", &hardware_probe, "dep:probe-rs");
    assert_feature_contains(
        "zeroclaw-hardware probe",
        &hardware_probe,
        "zeroclaw-tools/probe",
    );

    let tools_probe = feature_values(tools_features, "probe");
    assert_feature_contains("zeroclaw-tools probe", &tools_probe, "dep:probe-rs");

    let default_reachable = root_feature_reachable(root_features, &["default"]);
    assert_probe_boundary("root default", &default_reachable);

    let distribution_reachable = root_feature_reachable(root_features, &distribution_inputs(&root));
    assert_probe_boundary("standard distribution", &distribution_reachable);
}
