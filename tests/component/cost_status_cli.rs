use serde_json::{Value, json};
use std::path::Path;
use std::process::{Command, Output};

fn write_config(config_dir: &Path) {
    std::fs::write(
        config_dir.join("config.toml"),
        r#"locale = "en"

[cost]
enabled = true
daily_limit_usd = 10.0
monthly_limit_usd = 100.0
"#,
    )
    .unwrap();
}

fn usage_record(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
    pricing_available: Option<bool>,
) -> Value {
    let mut usage = json!({
        "model": model,
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "total_tokens": input_tokens + output_tokens,
        "cost_usd": cost_usd,
        "timestamp": chrono::Utc::now(),
    });
    if let Some(pricing_available) = pricing_available {
        usage["pricing_available"] = json!(pricing_available);
    }
    json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "usage": usage,
        "session_id": "status-test",
    })
}

fn run_status(records: &[Value]) -> Output {
    let config_dir = tempfile::tempdir().unwrap();
    write_config(config_dir.path());
    let state_dir = config_dir.path().join("data").join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let mut ledger = records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    ledger.push('\n');
    std::fs::write(state_dir.join("costs.jsonl"), ledger).unwrap();

    Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("RUST_LOG", "off")
        .arg("--config-dir")
        .arg(config_dir.path())
        .arg("status")
        .output()
        .expect("failed to run zeroclaw status")
}

fn output_text(output: &Output) -> (String, String) {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "zeroclaw status failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    (stdout, stderr)
}

#[test]
fn priced_status_fixture_has_spend_without_warning() {
    let output = run_status(&[usage_record("priced-model", 100, 50, 0.1, Some(true))]);
    let (stdout, stderr) = output_text(&output);
    assert!(stdout.contains("Spent today:       $0.1000 / $10.00"));
    assert!(!stderr.contains("Pricing unavailable"));
}

#[test]
fn configured_free_status_fixture_has_no_warning() {
    let output = run_status(&[usage_record("free-model", 100, 50, 0.0, Some(true))]);
    let (stdout, stderr) = output_text(&output);
    assert!(stdout.contains("Spent today:       $0.0000 / $10.00"));
    assert!(!stderr.contains("Pricing unavailable"));
}

#[test]
fn unpriced_status_fixture_warns_for_its_tokens() {
    let output = run_status(&[usage_record("unpriced-model", 100, 50, 0.0, Some(false))]);
    let (stdout, stderr) = output_text(&output);
    assert!(stdout.contains("Spent today:       $0.0000 / $10.00"));
    assert!(stderr.contains("Pricing unavailable for 1 model(s) (150 tokens uncosted)"));
    assert!(stderr.contains("unpriced-model"));
    assert!(stderr.contains("Add pricing to the active provider profile"));
}

#[test]
fn mixed_status_fixture_counts_only_the_unpriced_subset() {
    let output = run_status(&[
        usage_record("mixed-model", 100, 50, 0.1, Some(true)),
        usage_record("mixed-model", 75, 25, 0.0, Some(false)),
    ]);
    let (stdout, stderr) = output_text(&output);
    assert!(stdout.contains("Spent today:       $0.1000 / $10.00"));
    assert!(stderr.contains("Pricing unavailable for 1 model(s) (100 tokens uncosted)"));
    assert!(stderr.contains("mixed-model"));
    assert!(!stderr.contains("250 tokens uncosted"));
}

#[test]
fn legacy_status_fixture_keeps_missing_provenance_compatible() {
    let output = run_status(&[usage_record("legacy-model", 100, 50, 0.0, None)]);
    let (stdout, stderr) = output_text(&output);
    assert!(stdout.contains("Spent today:       $0.0000 / $10.00"));
    assert!(!stderr.contains("Pricing unavailable"));
}
