//! Component coverage for `zeroclaw eval run --mode live`.
//!
//! The live-mode unit tests inject a provider directly, so they never cross the
//! user-facing path: CLI parsing, config loading, `[eval].live_provider`
//! resolution, real provider construction, one completed model turn, and the
//! graded report. This launches the production binary against a local
//! OpenAI-compatible mock so that whole path is exercised without credentials
//! and without external network.

use std::process::Command;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ECHO_CASE: &str = r#"{
  "model_name": "live-cli-echo",
  "turns": [{ "user_input": "Reply with the word hello and nothing else." }],
  "expects": { "response_contains": ["hello"], "max_tool_calls": 0 }
}"#;

#[tokio::test]
async fn eval_run_live_drives_the_configured_provider_through_the_cli() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model": "mock-echo",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": { "role": "assistant", "content": "hello from the mock endpoint" }
            }],
            "usage": { "prompt_tokens": 11, "completion_tokens": 5, "total_tokens": 16 }
        })))
        .mount(&server)
        .await;

    let suite = tempfile::tempdir().expect("temporary suite directory");
    std::fs::write(suite.path().join("live_echo.json"), ECHO_CASE).expect("write live case");

    let config_dir = tempfile::tempdir().expect("temporary config directory");
    std::fs::write(
        config_dir.path().join("config.toml"),
        format!(
            r#"schema_version = 3

[reliability]
provider_retries = 0
provider_backoff_ms = 0

[risk_profiles.default]

[runtime_profiles.default]

[providers.models.custom.mock]
api_key = "test-key"
uri = "{}"
model = "mock-echo"
wire_api = "chat_completions"

[eval]
live_provider = "custom.mock"
case_timeout_secs = 30
"#,
            server.uri()
        ),
    )
    .expect("write test config");

    let config_dir_arg = config_dir.path().to_path_buf();
    let suite_arg = suite.path().to_path_buf();
    let output = std::thread::spawn(move || {
        Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
            .env("RUST_LOG", "off")
            .args([
                "--config-dir",
                config_dir_arg.to_str().expect("UTF-8 config path"),
                "eval",
                "run",
                "--suite",
                suite_arg.to_str().expect("UTF-8 suite path"),
                "--mode",
                "live",
                "--format",
                "json",
            ])
            .output()
            .expect("run zeroclaw eval")
    })
    .join()
    .expect("zeroclaw eval process must not panic");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "a passing live suite must exit 0\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let report: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("eval must print a JSON report");
    assert_eq!(
        report["all_passed"], true,
        "the live case must grade green: {report}"
    );
    assert_eq!(report["cases"][0]["name"], "live-cli-echo");

    // The request that actually left the process must carry the configured
    // model. A regression that drops the resolved metadata sends
    // `Agent::builder()`'s placeholder instead, which no injected-provider test
    // can catch because those providers ignore the model argument.
    let requests = server
        .received_requests()
        .await
        .expect("request recording enabled");
    let chat: Vec<_> = requests
        .iter()
        .filter(|r| r.url.path().ends_with("/chat/completions"))
        .collect();
    assert_eq!(
        chat.len(),
        1,
        "the live case must complete exactly one model turn"
    );
    let body: serde_json::Value =
        serde_json::from_slice(&chat[0].body).expect("provider request must be JSON");
    assert_eq!(
        body["model"], "mock-echo",
        "the configured model must reach the provider: {body}"
    );
}

/// Write an executable stand-in for a provider CLI that records its own
/// invocation, and return `(binary, marker)`.
#[cfg(unix)]
fn write_sentinel_binary(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let marker = dir.join("launched");
    let binary = dir.join("grok");
    std::fs::write(
        &binary,
        format!("#!/bin/sh\ntouch {}\nexit 0\n", marker.display()),
    )
    .expect("write sentinel binary");
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
        .expect("make the sentinel executable");
    (binary, marker)
}

/// Run the production binary against the given config and eval suite in live
/// mode, and return `(stdout, stderr, success)`.
#[cfg(unix)]
fn run_live_eval(config_toml: &str) -> (String, String, bool) {
    let suite = tempfile::tempdir().expect("temporary suite directory");
    std::fs::write(suite.path().join("live_echo.json"), ECHO_CASE).expect("write live case");

    let config_dir = tempfile::tempdir().expect("temporary config directory");
    std::fs::write(config_dir.path().join("config.toml"), config_toml).expect("write test config");

    let output = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("RUST_LOG", "off")
        .args([
            "--config-dir",
            config_dir.path().to_str().expect("UTF-8 config path"),
            "eval",
            "run",
            "--suite",
            suite.path().to_str().expect("UTF-8 suite path"),
            "--mode",
            "live",
            "--format",
            "json",
        ])
        .output()
        .expect("run zeroclaw eval");

    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

/// The provider guard must reject a CLI-backed `[eval].live_provider` before
/// anything is constructed, so the CLI binary is never executed. The sentinel
/// script records its own invocation, so a regression that moves the check
/// after provider construction leaves the marker behind and fails here.
#[cfg(unix)]
#[test]
fn eval_run_live_rejects_a_cli_backed_provider_without_launching_it() {
    let sentinel_dir = tempfile::tempdir().expect("temporary sentinel directory");
    let (binary, marker) = write_sentinel_binary(sentinel_dir.path());

    let (stdout, stderr, success) = run_live_eval(&format!(
        r#"schema_version = 3

[reliability]
provider_retries = 0
provider_backoff_ms = 0

[risk_profiles.default]

[runtime_profiles.default]

[providers.models.grok_cli.sentinel]
binary_path = "{}"
working_directory = "{}"
model = "grok-code-fast-1"

[eval]
live_provider = "grok_cli.sentinel"
case_timeout_secs = 30
"#,
        binary.display(),
        sentinel_dir.path().display()
    ));

    assert!(
        !success,
        "a CLI-backed live provider must fail the run\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !marker.exists(),
        "the CLI-backed provider must be refused before its binary is launched"
    );
    assert!(
        stderr.contains("live eval refuses the CLI-backed provider `grok_cli.sentinel`"),
        "the operator must be told which profile was refused and why\nstderr:\n{stderr}"
    );
}

/// A `fallback` entry is stored exactly as written and resolved by alias lookup
/// across every provider family, so a dotless entry can name a CLI-backed
/// profile without ever naming its family. The guard must resolve the entry the
/// way the factory does, refuse the run, and still keep the CLI out of the
/// process table; the primary here points at a closed port so a regression that
/// admits the config reaches the fallback.
#[cfg(unix)]
#[test]
fn eval_run_live_rejects_a_bare_fallback_ref_naming_a_cli_backed_profile() {
    let sentinel_dir = tempfile::tempdir().expect("temporary sentinel directory");
    let (binary, marker) = write_sentinel_binary(sentinel_dir.path());

    let (stdout, stderr, success) = run_live_eval(&format!(
        r#"schema_version = 3

[reliability]
provider_retries = 0
provider_backoff_ms = 0

[risk_profiles.default]

[runtime_profiles.default]

[providers.models.custom.mock]
api_key = "test-key"
uri = "http://127.0.0.1:9/v1"
model = "mock-echo"
wire_api = "chat_completions"
fallback = ["sentinel"]

[providers.models.grok_cli.sentinel]
binary_path = "{}"
working_directory = "{}"
model = "grok-code-fast-1"

[eval]
live_provider = "custom.mock"
case_timeout_secs = 30
"#,
        binary.display(),
        sentinel_dir.path().display()
    ));

    assert!(
        !success,
        "a CLI-backed fallback must fail the run\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !marker.exists(),
        "the CLI-backed fallback must be refused before its binary is launched"
    );
    assert!(
        stderr.contains("live eval refuses the CLI-backed provider `grok_cli.sentinel`"),
        "the operator must be told which profile was refused and why\nstderr:\n{stderr}"
    );
}
