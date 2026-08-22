//! Handoff: the launcher only hands off to a server whose identity it checked.
//!
//! The stub server is a shell script, so these tests are Unix-only. The
//! parsing and version-range rules they exercise are platform-independent and
//! are additionally covered by the unit tests in `src/handoff.rs`.

#![cfg(unix)]

mod support;

use support::{control_server_stub, initialize_response};

use zeroclaw_bootstrap::error::BootstrapError;
use zeroclaw_bootstrap::handoff;

#[test]
fn verifies_a_well_formed_advertisement() {
    let dir = tempfile::tempdir().expect("temp dir");
    let stub = control_server_stub(
        dir.path(),
        "zeroclaw",
        "0.8.4",
        &initialize_response("0.8.4", "1.0"),
    );

    let verified = handoff::verify(&stub, None).expect("advertisement must verify");
    assert_eq!(verified.advertisement.zeroclaw_version, "0.8.4");
    assert_eq!(verified.advertisement.control_protocol_version, "1.0");
    assert_eq!(verified.advertisement.config_schema_version, 3);
    assert_eq!(
        verified.advertisement.capabilities,
        vec!["agents".to_string()]
    );
    assert!(
        verified
            .advertisement
            .capability_digest
            .starts_with("sha256:")
    );
    assert_eq!(verified.verified_version, "0.8.4");
    assert_eq!(verified.binary_digest.len(), 64);

    let rendered = verified.render();
    assert!(rendered.contains("control protocol  1.0"));
    assert!(rendered.contains("capability digest sha256:"));
}

#[test]
fn accepts_a_higher_supported_minor_version() {
    let dir = tempfile::tempdir().expect("temp dir");
    let stub = control_server_stub(
        dir.path(),
        "zeroclaw",
        "0.9.0",
        &initialize_response("0.9.0", "1.4"),
    );
    let verified = handoff::verify(&stub, None).expect("a higher minor is additive");
    assert_eq!(verified.advertisement.control_protocol_version, "1.4");
}

#[test]
fn refuses_a_control_protocol_outside_the_supported_range() {
    for protocol in ["2.0", "0.9", "3.7"] {
        let dir = tempfile::tempdir().expect("temp dir");
        let stub = control_server_stub(
            dir.path(),
            "zeroclaw",
            "0.8.4",
            &initialize_response("0.8.4", protocol),
        );
        let err = handoff::verify(&stub, None).expect_err("must fail closed");
        match err {
            BootstrapError::UnsupportedProtocolVersion { advertised, .. } => {
                assert_eq!(advertised, protocol);
            }
            other => panic!("protocol `{protocol}` produced {other:?}"),
        }
    }
}

#[test]
fn refuses_an_advertisement_missing_a_required_field() {
    for field in [
        "zeroclaw_version",
        "control_protocol_version",
        "config_schema_version",
        "capabilities",
        "capability_digest",
    ] {
        let mut response: serde_json::Value =
            serde_json::from_str(&initialize_response("0.8.4", "1.0")).expect("fixture json");
        response["result"]["_meta"]["zeroclaw_control"]
            .as_object_mut()
            .expect("object")
            .remove(field);

        let dir = tempfile::tempdir().expect("temp dir");
        let stub = control_server_stub(dir.path(), "zeroclaw", "0.8.4", &response.to_string());
        let err = handoff::verify(&stub, None).expect_err("must refuse");
        assert!(
            matches!(err, BootstrapError::AdvertisementIncomplete { .. }),
            "missing `{field}` produced {err:?}"
        );
    }
}

#[test]
fn refuses_a_server_with_no_advertisement_carrier() {
    let bare = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "serverInfo": { "name": "not-zeroclaw-control", "version": "0.8.4" }
        }
    })
    .to_string();

    let dir = tempfile::tempdir().expect("temp dir");
    let stub = control_server_stub(dir.path(), "zeroclaw", "0.8.4", &bare);
    let err = handoff::verify(&stub, None).expect_err("must refuse");
    assert!(
        matches!(
            err,
            BootstrapError::AdvertisementIncomplete {
                field: "result._meta.zeroclaw_control"
            }
        ),
        "{err:?}"
    );
}

#[test]
fn refuses_when_the_binary_and_the_advertisement_disagree_on_the_product_version() {
    let dir = tempfile::tempdir().expect("temp dir");
    // The binary reports 0.8.4; the server claims to be 0.9.9.
    let stub = control_server_stub(
        dir.path(),
        "zeroclaw",
        "0.8.4",
        &initialize_response("0.9.9", "1.0"),
    );
    let err = handoff::verify(&stub, None).expect_err("must refuse");
    assert!(
        matches!(err, BootstrapError::ProductVersionMismatch { .. }),
        "{err:?}"
    );
}

#[test]
fn refuses_a_binary_that_is_not_the_artifact_the_launcher_installed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let stub = control_server_stub(
        dir.path(),
        "zeroclaw",
        "0.8.4",
        &initialize_response("0.8.4", "1.0"),
    );

    let wrong = "0".repeat(64);
    let err = handoff::verify(&stub, Some(&wrong)).expect_err("must refuse");
    assert!(
        matches!(err, BootstrapError::ExecutableIdentityMismatch { .. }),
        "{err:?}"
    );

    // The same probe with the real digest is accepted.
    let actual = zeroclaw_bootstrap::fetch::sha256_file(&stub).expect("digest");
    assert!(handoff::verify(&stub, Some(&actual)).is_ok());
}

#[test]
fn refuses_a_handoff_target_that_does_not_exist() {
    let dir = tempfile::tempdir().expect("temp dir");
    let err = handoff::verify(&dir.path().join("zeroclaw"), None).expect_err("must refuse");
    assert!(
        matches!(err, BootstrapError::HandoffTargetUnusable { .. }),
        "{err:?}"
    );
}

#[test]
fn refuses_a_binary_whose_version_cannot_be_established() {
    let dir = tempfile::tempdir().expect("temp dir");
    support::write_executable(
        dir.path(),
        "zeroclaw",
        "#!/bin/sh\necho 'some-other-tool 1.0'\nexit 0\n",
    );
    let err = handoff::verify(&dir.path().join("zeroclaw"), None).expect_err("must refuse");
    assert!(
        matches!(err, BootstrapError::HandoffTargetUnusable { .. }),
        "{err:?}"
    );
}

#[test]
fn refuses_a_server_that_answers_with_a_json_rpc_error() {
    let error_response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": { "code": -32600, "message": "unsupported_protocol_version" }
    })
    .to_string();

    let dir = tempfile::tempdir().expect("temp dir");
    let stub = control_server_stub(dir.path(), "zeroclaw", "0.8.4", &error_response);
    let err = handoff::verify(&stub, None).expect_err("must refuse");
    assert!(
        matches!(err, BootstrapError::HandoffProbeFailed { .. }),
        "{err:?}"
    );
}
