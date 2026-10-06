//! Enforces the public-consumer dependency closure and generic construction boundary.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .unwrap()
        .to_path_buf()
}

// Count balanced blocks after a function signature. Rust formatting/line numbers
// do not affect this check; concrete construction cannot be waved through by a
// retained manifest edge. Known entry bodies use balanced braces in literals too.
fn function<'a>(source: &'a str, name: &str) -> &'a str {
    let marker = format!("fn {name}(");
    let start = source
        .find(&marker)
        .unwrap_or_else(|| panic!("missing boundary function {name}"));
    let open = start + source[start..].find('{').unwrap();
    let mut depth = 0;
    for (offset, character) in source[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return &source[open..=open + offset];
        }
    }
    panic!("unclosed boundary function {name}");
}

#[test]
fn dependency_boundary() {
    let root = root();
    let output = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args([
            "metadata",
            "--format-version=1",
            "--no-deps",
            "--offline",
            "--locked",
        ])
        .output()
        .expect("native Cargo metadata must run");
    assert!(
        output.status.success(),
        "Cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
    let packages = metadata["packages"].as_array().unwrap();
    let package = |name: &str| packages.iter().find(|p| p["name"] == name).unwrap();
    let consumer = package("runtime-composition-consumer");
    let permitted: BTreeSet<&str> = [
        "zeroclaw-runtime",
        "zeroclaw-api",
        "zeroclaw-config",
        "anyhow",
        "async-trait",
        "serde_json",
        "tempfile",
        "tokio",
    ]
    .into_iter()
    .collect();
    for dependency in consumer["dependencies"].as_array().unwrap() {
        let name = dependency["name"].as_str().unwrap();
        assert!(
            permitted.contains(name),
            "consumer imported implementation crate {name}"
        );
        assert!(
            !dependency["features"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "test-util"),
            "consumer enabled private test-util"
        );
        if name == "zeroclaw-runtime" {
            assert_eq!(
                dependency["uses_default_features"], false,
                "consumer must prove the public runtime contract alone"
            );
        }
    }
    let policy: Value =
        serde_json::from_str(include_str!("../retained-dependencies.json")).unwrap();
    let runtime = package("zeroclaw-runtime");
    let mut observed = BTreeMap::<String, BTreeSet<String>>::new();
    for dependency in runtime["dependencies"].as_array().unwrap() {
        let name = dependency["name"].as_str().unwrap();
        if !name.starts_with("zeroclaw-") || dependency["kind"] == "dev" {
            continue;
        }
        let kind = dependency["kind"].as_str().unwrap_or("normal");
        let retained = &policy[kind][name];
        assert!(
            retained.is_object(),
            "unapproved runtime {kind} dependency {name}"
        );
        let owner = retained["owner"].as_str().unwrap();
        assert!(
            root.join(owner).is_file(),
            "retained owner is not active: {owner}"
        );
        let reason = retained["reason"].as_str().unwrap();
        assert!(
            reason.len() >= 50,
            "retained edge {name} has no concrete explanation"
        );
        println!("retained {kind} {name}: {owner}: {reason}");
        observed.entry(kind.into()).or_default().insert(name.into());
    }
    for kind in ["normal", "build"] {
        let expected: BTreeSet<String> =
            policy[kind].as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            observed.remove(kind).unwrap_or_default(),
            expected,
            "retained {kind} policy drift"
        );
    }
    let forbidden = [
        "tools::all_tools_",
        "zeroclaw_providers::create_",
        "providers::create_",
        "zeroclaw_memory::create_",
        "RuntimeCapabilities::config_backed",
        "Tool::new",
        "CLI_CHANNEL_FN.get",
        "load_peripheral_tools(",
    ];
    let boundaries = [
        (
            "crates/zeroclaw-runtime/src/agent/loop_.rs",
            "run_with_capabilities",
        ),
        (
            "crates/zeroclaw-runtime/src/agent/loop_.rs",
            "process_message_inner",
        ),
        (
            "crates/zeroclaw-runtime/src/agent/agent.rs",
            "build_with_capabilities",
        ),
        (
            "crates/zeroclaw-runtime/src/agent/turn/mod.rs",
            "assemble_owned_execution_with_capabilities",
        ),
    ];
    let mut violations = Vec::new();
    for (path, name) in boundaries {
        let source = std::fs::read_to_string(root.join(path)).unwrap();
        let body = function(&source, name);
        let compact: String = body
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        for constructor in forbidden {
            if compact.contains(constructor) {
                violations.push(format!("{path}::{name}: {constructor}"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "generic construction boundary violated (retained compatibility edges are not waivers):\n{}",
        violations.join("\n")
    );
}
