//! Architecture gate: only the `gateway` feature brings the HTTP gateway into
//! the `zeroclaw` binary.
//!
//! Other root features forward sub-features to `zeroclaw-gateway` so that a
//! build which already has the gateway gets the matching behaviour (metrics
//! scraping, embedded dashboard assets, channel webhooks). Written as a strong
//! edge (`zeroclaw-gateway/<feature>`), such a forward also switches the
//! optional dependency on, so a headless build that selects metrics or the
//! embedded dashboard silently links the whole HTTP server. Forwards must use
//! the weak form (`zeroclaw-gateway?/<feature>`), which applies only when
//! `gateway` is already selected.

use std::fs;
use std::path::Path;

const GATEWAY_CRATE: &str = "zeroclaw-gateway";
const GATEWAY_FEATURE: &str = "gateway";

fn root_manifest() -> toml::Table {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(root.join("Cargo.toml"))
        .expect("read root Cargo.toml")
        .parse()
        .expect("root Cargo.toml is valid TOML")
}

/// How one feature entry reaches the gateway crate, if it does.
#[derive(Debug, PartialEq, Eq)]
enum GatewayEdge {
    /// `dep:zeroclaw-gateway` or a bare `zeroclaw-gateway`: enables the crate.
    Enables,
    /// `zeroclaw-gateway/<feature>`: enables the crate and the feature.
    Strong,
    /// `zeroclaw-gateway?/<feature>`: applies only when the crate is enabled.
    Weak,
}

fn gateway_edge(entry: &str) -> Option<GatewayEdge> {
    if entry == GATEWAY_CRATE || entry.strip_prefix("dep:") == Some(GATEWAY_CRATE) {
        return Some(GatewayEdge::Enables);
    }
    let (dependency, _feature) = entry.split_once('/')?;
    if dependency == GATEWAY_CRATE {
        Some(GatewayEdge::Strong)
    } else if dependency.strip_suffix('?') == Some(GATEWAY_CRATE) {
        Some(GatewayEdge::Weak)
    } else {
        None
    }
}

#[test]
fn gateway_edge_classifies_every_entry_form() {
    assert_eq!(
        gateway_edge("dep:zeroclaw-gateway"),
        Some(GatewayEdge::Enables)
    );
    assert_eq!(gateway_edge("zeroclaw-gateway"), Some(GatewayEdge::Enables));
    assert_eq!(
        gateway_edge("zeroclaw-gateway/embedded-web"),
        Some(GatewayEdge::Strong)
    );
    assert_eq!(
        gateway_edge("zeroclaw-gateway?/embedded-web"),
        Some(GatewayEdge::Weak)
    );
    assert_eq!(gateway_edge("zeroclaw-gateway-extra/x"), None);
    assert_eq!(gateway_edge("zeroclaw-runtime/webauthn"), None);
    assert_eq!(gateway_edge(GATEWAY_FEATURE), None);
}

#[test]
fn only_the_gateway_feature_enables_the_gateway_crate() {
    let manifest = root_manifest();

    let dependency = manifest["dependencies"][GATEWAY_CRATE]
        .as_table()
        .expect("zeroclaw-gateway is declared as a dependency table");
    assert_eq!(
        dependency.get("optional").and_then(toml::Value::as_bool),
        Some(true),
        "zeroclaw-gateway must stay an optional dependency of the root crate"
    );

    let features = manifest["features"]
        .as_table()
        .expect("root Cargo.toml has a [features] table");
    let gateway_entries = features[GATEWAY_FEATURE]
        .as_array()
        .expect("the gateway feature is a list");
    assert!(
        gateway_entries
            .iter()
            .filter_map(toml::Value::as_str)
            .any(|entry| gateway_edge(entry) == Some(GatewayEdge::Enables)),
        "the gateway feature must enable the zeroclaw-gateway dependency"
    );

    let mut violations = Vec::new();
    for (feature, entries) in features {
        if feature == GATEWAY_FEATURE {
            continue;
        }
        let entries = entries
            .as_array()
            .unwrap_or_else(|| panic!("feature `{feature}` is a list"));
        for entry in entries.iter().filter_map(toml::Value::as_str) {
            match gateway_edge(entry) {
                Some(GatewayEdge::Enables | GatewayEdge::Strong) => {
                    violations.push(format!("{feature} = \"{entry}\""));
                }
                Some(GatewayEdge::Weak) | None => {}
            }
        }
    }

    assert!(
        violations.is_empty(),
        "these root features switch on the gateway crate without the `gateway` \
         feature; forward with `{GATEWAY_CRATE}?/<feature>` instead, or select \
         `{GATEWAY_FEATURE}` explicitly:\n  {}",
        violations.join("\n  ")
    );
}
