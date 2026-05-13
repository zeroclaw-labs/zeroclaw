//! Regression coverage for `zeroclaw config patch --json` error output.
//!
//! This intentionally exercises the CLI process boundary instead of spinning up
//! the gateway. HTTP parity comes from using the same `ConfigApiError` envelope
//! shape and field semantics as `PATCH /api/config`; a gateway fixture would add
//! auth/server setup without increasing coverage for this CLI-only regression.

use std::process::{Command, Stdio};

#[test]
fn config_patch_json_failed_op_emits_structured_error_envelope() {
    let bin = env!("CARGO_BIN_EXE_zeroclaw");
    let config_dir = tempfile::tempdir().expect("temp config dir");

    let output = Command::new(bin)
        .env("ZEROCLAW_CONFIG_DIR", config_dir.path())
        .env("RUST_LOG", "off")
        .args(["config", "patch", "--json", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            {
                use std::io::Write;
                child
                    .stdin
                    .as_mut()
                    .expect("child stdin")
                    .write_all(br#"[{"op":"replace","path":"/not/a/path","value":"x"}]"#)?;
            }
            child.wait_with_output()
        })
        .expect("run zeroclaw config patch");

    assert!(!output.status.success(), "patch should fail");
    assert!(
        output.stdout.is_empty(),
        "failed --json patch should not emit success stdout: {}",
        String::from_utf8_lossy(&output.stdout),
    );

    let stderr = String::from_utf8(output.stderr).expect("stderr utf8");
    let envelope: serde_json::Value =
        serde_json::from_str(&stderr).expect("stderr should be JSON error envelope");
    assert_eq!(envelope["code"], "path_not_found");
    assert_eq!(envelope["path"], "not.a.path");
    assert_eq!(envelope["op_index"], 0);
    assert!(
        envelope["message"]
            .as_str()
            .expect("message")
            .contains("not.a.path"),
        "message should identify path: {envelope}"
    );
}
