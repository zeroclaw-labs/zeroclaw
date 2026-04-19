#![cfg(feature = "plugins-wasm")]

//! Integration test: sensitive config values are redacted in log output.
//!
//! Task US-ZCL-7-9: Ensure sensitive values are never logged.
//!
//! Verifies that when `resolve_plugin_config` logs config resolution,
//! keys marked `sensitive = true` in the manifest have their values
//! replaced by redacted output from `redact()`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;
use zeroclaw::plugins::{is_sensitive_key, resolve_plugin_config};
use zeroclaw::security::redact;

/// A writer that captures all output into a shared buffer.
#[derive(Clone)]
struct CaptureWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl CaptureWriter {
    fn new() -> Self {
        Self {
            buf: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn contents(&self) -> String {
        let buf = self.buf.lock().unwrap();
        String::from_utf8_lossy(&buf).to_string()
    }
}

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Helper: run `resolve_plugin_config` under a tracing subscriber that captures output,
/// then return the captured log text.
fn resolve_with_captured_logs(
    plugin_name: &str,
    manifest_config: &HashMap<String, serde_json::Value>,
    config_values: Option<&HashMap<String, String>>,
) -> (
    Result<std::collections::BTreeMap<String, String>, zeroclaw::plugins::error::PluginError>,
    String,
) {
    let writer = CaptureWriter::new();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(writer.clone())
        .finish();

    let result = tracing::subscriber::with_default(subscriber, || {
        resolve_plugin_config(plugin_name, manifest_config, config_values)
    });

    (result, writer.contents())
}

/// Sensitive values supplied in operator config are redacted in log output.
#[test]
fn sensitive_config_value_is_redacted_in_logs() {
    let mut manifest_config: HashMap<String, serde_json::Value> = HashMap::new();
    manifest_config.insert(
        "api_key".to_string(),
        serde_json::json!({"required": true, "sensitive": true}),
    );
    manifest_config.insert("region".to_string(), serde_json::json!({"required": true}));

    let secret = "sk-live-super-secret-key-abc123";
    let mut config_values: HashMap<String, String> = HashMap::new();
    config_values.insert("api_key".to_string(), secret.to_string());
    config_values.insert("region".to_string(), "us-east-1".to_string());

    let (result, logs) =
        resolve_with_captured_logs("test-plugin", &manifest_config, Some(&config_values));
    assert!(result.is_ok(), "config resolution should succeed");

    // The raw secret must NOT appear anywhere in the logs.
    assert!(
        !logs.contains(secret),
        "log output must not contain the raw sensitive value '{secret}', got:\n{logs}"
    );

    // The redacted form should appear instead.
    let redacted = redact(secret);
    assert!(
        logs.contains(&redacted),
        "log output should contain the redacted value '{redacted}', got:\n{logs}"
    );

    // Non-sensitive values can appear in logs.
    assert!(
        logs.contains("us-east-1"),
        "log output should contain non-sensitive value 'us-east-1', got:\n{logs}"
    );
}

/// Sensitive default values from the manifest are also redacted.
#[test]
fn sensitive_default_value_is_redacted_in_logs() {
    let mut manifest_config: HashMap<String, serde_json::Value> = HashMap::new();
    manifest_config.insert(
        "internal_token".to_string(),
        serde_json::json!({"default": "tk-internal-fallback-secret-999", "sensitive": true}),
    );

    let (result, logs) = resolve_with_captured_logs("default-plugin", &manifest_config, None);
    assert!(result.is_ok(), "config resolution should succeed");

    let default_secret = "tk-internal-fallback-secret-999";
    assert!(
        !logs.contains(default_secret),
        "log output must not contain the raw sensitive default value, got:\n{logs}"
    );

    let redacted = redact(default_secret);
    assert!(
        logs.contains(&redacted),
        "log output should contain the redacted default '{redacted}', got:\n{logs}"
    );
}

/// Non-sensitive keys are logged with their actual values.
#[test]
fn non_sensitive_config_value_appears_in_logs() {
    let mut manifest_config: HashMap<String, serde_json::Value> = HashMap::new();
    manifest_config.insert("model".to_string(), serde_json::json!({"default": "gpt-4"}));

    let (result, logs) = resolve_with_captured_logs("plain-plugin", &manifest_config, None);
    assert!(result.is_ok(), "config resolution should succeed");

    assert!(
        logs.contains("gpt-4"),
        "log output should contain non-sensitive default value 'gpt-4', got:\n{logs}"
    );
}

/// The `is_sensitive_key` helper correctly identifies sensitive declarations.
#[test]
fn is_sensitive_key_recognizes_sensitive_flag() {
    assert!(is_sensitive_key(&serde_json::json!({"sensitive": true})));
    assert!(is_sensitive_key(
        &serde_json::json!({"required": true, "sensitive": true})
    ));
    assert!(is_sensitive_key(
        &serde_json::json!({"default": "x", "sensitive": true})
    ));

    assert!(!is_sensitive_key(&serde_json::json!({"required": true})));
    assert!(!is_sensitive_key(&serde_json::json!({"sensitive": false})));
    assert!(!is_sensitive_key(&serde_json::json!("bare-string")));
    assert!(!is_sensitive_key(&serde_json::json!(42)));
}
