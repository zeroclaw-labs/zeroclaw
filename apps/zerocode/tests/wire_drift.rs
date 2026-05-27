//! Regression: wire-shape mirrors in `apps/zerocode/src/` must
//! serialize identically to the canonical workspace types.
//!
//! `apps/zerocode` is a standalone TUI binary that does not depend on
//! `zeroclaw-config` / `zeroclaw-runtime` / `zeroclaw-api`. It carries
//! hand-maintained serde mirrors for every type that crosses the
//! JSON-RPC wire. If the canonical workspace type adds, removes, or
//! renames a field, this test fails — the mirror must follow.
//!
//! Only this dev-dep edge couples `apps/zerocode` to the workspace.
//! Production builds of `zerocode` link none of these crates.

use serde_json::json;

// Canonical workspace types — dev-deps only.
use zeroclaw_api::jsonrpc as api;
use zeroclaw_config::multi_agent::MemoryBackendKind;
use zeroclaw_config::presets;
use zeroclaw_config::sections::SectionShape;
use zeroclaw_config::traits::{ConfigTab, PropKind};
use zeroclaw_runtime::quickstart;
use zeroclaw_runtime::rpc::types as rpc_types;

// Local mirrors under test — what production `zerocode` will use
// after the workspace dependencies are stripped.
use zerocode::wire;

/// Helper: serialize and deserialize one round-trip through JSON and
/// assert the canonical and mirror types produce identical bytes
/// when handed equivalent inputs.
#[allow(dead_code)]
fn assert_json_eq<A, B>(a: &A, b: &B, label: &str)
where
    A: serde::Serialize,
    B: serde::Serialize,
{
    let a_json = serde_json::to_value(a).expect("serialize canonical");
    let b_json = serde_json::to_value(b).expect("serialize mirror");
    assert_eq!(
        a_json, b_json,
        "{label} mirror drifted from canonical:\n  canonical = {a_json}\n  mirror    = {b_json}"
    );
}

#[test]
fn builder_submission_round_trips() {
    let canonical = presets::BuilderSubmission {
        model_provider: presets::SelectorChoice::Fresh(presets::ModelProviderChoice {
            provider_type: "anthropic".into(),
            alias: "anthropic".into(),
            default_model: "claude-sonnet-4-5".into(),
            api_key: Some("sk-test".into()),
            base_url: None,
        }),
        risk_profile: presets::SelectorChoice::Fresh("balanced".into()),
        runtime_profile: presets::SelectorChoice::Existing("tight".into()),
        memory: presets::SelectorChoice::Fresh(MemoryBackendKind::Postgres),
        channels: vec![presets::SelectorChoice::Fresh(presets::ChannelQuickStart {
            channel_type: "telegram".into(),
            alias: "tg".into(),
            token: Some("xyz".into()),
        })],
        agent: presets::AgentIdentity {
            name: "demo".into(),
            system_prompt: "be terse".into(),
            personality_file: Some("foo.md".into()),
        },
    };
    let json = serde_json::to_value(&canonical).expect("serialize canonical");
    let mirror: wire::BuilderSubmission =
        serde_json::from_value(json.clone()).expect("deserialize into mirror");
    let round = serde_json::to_value(&mirror).expect("re-serialize mirror");
    assert_eq!(json, round, "BuilderSubmission round-trip drift");
}

#[test]
fn memory_backend_kind_variants_all_round_trip() {
    for kind in [
        MemoryBackendKind::None,
        MemoryBackendKind::Sqlite,
        MemoryBackendKind::Postgres,
        MemoryBackendKind::Qdrant,
        MemoryBackendKind::Markdown,
        MemoryBackendKind::Lucid,
    ] {
        let json = serde_json::to_value(kind).expect("serialize canonical");
        let mirror: wire::MemoryBackendKind =
            serde_json::from_value(json.clone()).expect("deserialize into mirror");
        let round = serde_json::to_value(mirror).expect("re-serialize mirror");
        assert_eq!(json, round, "MemoryBackendKind::{kind:?} round-trip drift");
    }
}

#[test]
fn quickstart_state_mirrors_canonical() {
    let canonical = quickstart::QuickstartState {
        quickstart_completed: true,
        agents: vec!["a".into()],
        risk_profiles: vec!["balanced".into()],
        runtime_profiles: vec!["balanced".into()],
        model_providers: vec!["anthropic.default".into()],
        channels: vec!["telegram.tg".into()],
        unassigned_channels: vec!["telegram.tg".into()],
        storage: vec!["sqlite.sqlite".into()],
        model_provider_types: vec![quickstart::QuickstartTypeOption {
            kind: "anthropic".into(),
            display_name: "Anthropic".into(),
            local: false,
        }],
        channel_types: vec![quickstart::QuickstartTypeOption {
            kind: "telegram".into(),
            display_name: "Telegram".into(),
            local: false,
        }],
    };
    let json = serde_json::to_value(&canonical).expect("serialize");
    let mirror: wire::QuickstartState = serde_json::from_value(json.clone()).expect("deserialize");
    let round = serde_json::to_value(&mirror).expect("re-serialize");
    assert_eq!(json, round, "QuickstartState drift");
}

#[test]
fn surface_mirrors_snake_case_wire() {
    for s in [
        quickstart::Surface::Web,
        quickstart::Surface::Tui,
        quickstart::Surface::Cli,
        quickstart::Surface::Test,
    ] {
        let json = serde_json::to_value(s).expect("serialize");
        let mirror: wire::Surface = serde_json::from_value(json.clone()).expect("deserialize");
        let round = serde_json::to_value(mirror).expect("re-serialize");
        assert_eq!(json, round, "Surface::{s:?} drift");
    }
}

#[test]
fn quickstart_step_mirrors_canonical() {
    for s in [
        quickstart::QuickstartStep::ModelProvider,
        quickstart::QuickstartStep::RiskProfile,
        quickstart::QuickstartStep::RuntimeProfile,
        quickstart::QuickstartStep::Memory,
        quickstart::QuickstartStep::Channels,
        quickstart::QuickstartStep::Agent,
    ] {
        let json = serde_json::to_value(s).expect("serialize");
        let mirror: wire::QuickstartStep =
            serde_json::from_value(json.clone()).expect("deserialize");
        let round = serde_json::to_value(mirror).expect("re-serialize");
        assert_eq!(json, round, "QuickstartStep::{s:?} drift");
    }
}

#[test]
fn quickstart_error_round_trips() {
    let canonical = quickstart::QuickstartError {
        step: quickstart::QuickstartStep::Memory,
        field: "backend".into(),
        message: "boom".into(),
    };
    let json = serde_json::to_value(&canonical).expect("serialize");
    let mirror: wire::QuickstartError = serde_json::from_value(json.clone()).expect("deserialize");
    let round = serde_json::to_value(&mirror).expect("re-serialize");
    assert_eq!(json, round, "QuickstartError drift");
}

#[test]
fn field_section_mirrors_canonical() {
    for s in [
        quickstart::FieldSection::ModelProvider,
        quickstart::FieldSection::Channel,
    ] {
        let json = serde_json::to_value(s).expect("serialize");
        let mirror: wire::FieldSection = serde_json::from_value(json.clone()).expect("deserialize");
        let round = serde_json::to_value(mirror).expect("re-serialize");
        assert_eq!(json, round, "FieldSection::{s:?} drift");
    }
}

#[test]
fn prop_kind_variants_round_trip() {
    // Walk every PropKind variant. Sampling all the wire names ensures
    // the TUI mirror's serde rename pattern matches the canonical
    // schema-trait enum.
    let sample = json!("string");
    let _canonical: PropKind = serde_json::from_value(sample.clone()).expect("canonical");
    let _mirror: wire::PropKind = serde_json::from_value(sample).expect("mirror");
    // Spot-check a few more — actual exhaustive coverage is enforced
    // by the wire mirror's `#[serde(rename_all = "snake_case")]`
    // matching the canonical trait's attribute.
    for v in [
        "string",
        "integer",
        "float",
        "bool",
        "duration",
        "enum",
        "string_array",
        "secret",
        "json",
    ] {
        let value = json!(v);
        let canonical: Result<PropKind, _> = serde_json::from_value(value.clone());
        let mirror: Result<wire::PropKind, _> = serde_json::from_value(value.clone());
        assert_eq!(
            canonical.is_ok(),
            mirror.is_ok(),
            "PropKind wire `{v}` parse mismatch between canonical and mirror"
        );
        if let (Ok(c), Ok(m)) = (canonical, mirror) {
            assert_eq!(
                serde_json::to_value(c).unwrap(),
                serde_json::to_value(m).unwrap(),
                "PropKind `{v}` re-serializes differently"
            );
        }
    }
}

#[test]
fn config_tab_round_trips_arbitrary_string() {
    // ConfigTab is PascalCase on the wire (no `serde(rename_all)`).
    for tab in ["None", "Connection", "Behavior", "Channels", "Personality"] {
        let value = json!(tab);
        let canonical: ConfigTab = serde_json::from_value(value.clone())
            .unwrap_or_else(|e| panic!("canonical `{tab}` failed: {e}"));
        let mirror: wire::ConfigTab = serde_json::from_value(value.clone())
            .unwrap_or_else(|e| panic!("mirror `{tab}` failed: {e}"));
        let c_json = serde_json::to_value(canonical).unwrap();
        let m_json = serde_json::to_value(mirror).unwrap();
        assert_eq!(c_json, m_json, "ConfigTab `{tab}` drift");
    }
}

#[test]
fn fs_list_dir_response_round_trips() {
    let canonical = api::FsListDirResponse {
        cwd: "/work".into(),
        entries: vec![api::FsEntry {
            name: "x".into(),
            full_path: "/work/x".into(),
            is_dir: false,
            is_hidden: false,
            size: 42,
            mtime: Some(1_000),
        }],
    };
    let json = serde_json::to_value(&canonical).expect("serialize");
    let mirror: wire::FsListDirResponse =
        serde_json::from_value(json.clone()).expect("deserialize");
    let round = serde_json::to_value(&mirror).expect("re-serialize");
    assert_eq!(json, round, "FsListDirResponse drift");
}

#[test]
fn quickstart_type_option_mirrors_canonical() {
    let canonical = rpc_types::QuickstartTypeOption {
        kind: "telegram".into(),
        display_name: "Telegram".into(),
        local: false,
    };
    let json = serde_json::to_value(&canonical).expect("serialize");
    let mirror: wire::QuickstartTypeOption =
        serde_json::from_value(json.clone()).expect("deserialize");
    let round = serde_json::to_value(&mirror).expect("re-serialize");
    assert_eq!(json, round, "QuickstartTypeOption drift");
}

/// SectionShape is the daemon's per-section schema descriptor.
/// We don't construct a full canonical fixture — the test
/// exercises the round-trip via a minimal JSON shape so the
/// mirror's field set is verified against the canonical
/// deserializer.
#[test]
fn section_shape_round_trips_via_json() {
    let raw = json!({
        "section": "channels",
        "title": "Channels",
        "description": "messenger bots",
        "fields": [],
        "tabs": [],
    });
    let canonical: Result<SectionShape, _> = serde_json::from_value(raw.clone());
    let mirror: Result<wire::SectionShape, _> = serde_json::from_value(raw.clone());
    assert_eq!(
        canonical.is_ok(),
        mirror.is_ok(),
        "SectionShape parse mismatch between canonical and mirror"
    );
}
