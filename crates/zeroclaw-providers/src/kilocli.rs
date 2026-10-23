//! KiloCLI subprocess model_provider.
use crate::traits::{ChatRequest, ChatResponse, ModelProvider};
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::process::Command;
use tokio::time::Duration;

/// Default `kilo` binary name (resolved via `PATH`).
const DEFAULT_KILO_CLI_BINARY: &str = "kilo";

/// Model name used to signal "use the model_provider's own default model".
const DEFAULT_MODEL_MARKER: &str = "default";
/// KiloCLI requests are bounded to avoid hung subprocesses.
const KILO_CLI_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// The CLI does not support sampling controls; allow only baseline defaults.
const KILO_CLI_SUPPORTED_TEMPERATURES: [f64; 2] = [0.7, 1.0];
const TEMP_EPSILON: f64 = 1e-9;

/// ModelProvider that invokes the KiloCLI as a subprocess.
/// Each inference request spawns a fresh `kilo` process. This is the
/// non-interactive approach: the process handles the prompt and exits.
pub struct KiloCliModelProvider {
    /// `[providers.models.<family>.<alias>]` config-key alias.
    alias: String,
    /// Path to the `kilo` binary.
    binary_path: PathBuf,
}

/// Typed builder for [`KiloCliModelProvider`].
///
/// Only `alias` is required; the binary path defaults to
/// `DEFAULT_KILO_CLI_BINARY` (PATH lookup) when unset.
#[must_use]
pub struct KiloCliBuilder {
    alias: String,
    binary_path: Option<String>,
}

impl KiloCliBuilder {
    /// Override the `kilo` CLI binary path. Whitespace-only inputs fall
    /// back to the default (PATH lookup).
    pub fn binary_path(mut self, path: Option<&str>) -> Self {
        self.binary_path = path
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        self
    }

    pub fn build(self) -> KiloCliModelProvider {
        let binary_path = self
            .binary_path
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_KILO_CLI_BINARY));
        KiloCliModelProvider {
            alias: self.alias,
            binary_path,
        }
    }
}

impl KiloCliModelProvider {
    /// Entry point. Only `alias` is required; every other field is set
    /// via a labelled chain method on the returned [`KiloCliBuilder`].
    pub fn builder(alias: &str) -> KiloCliBuilder {
        KiloCliBuilder {
            alias: alias.to_string(),
            binary_path: None,
        }
    }

    /// Returns true if the model argument should be forwarded to the CLI.
    fn should_forward_model(model: &str) -> bool {
        let trimmed = model.trim();
        !trimmed.is_empty() && trimmed != DEFAULT_MODEL_MARKER
    }

    fn supports_temperature(temperature: f64) -> bool {
        KILO_CLI_SUPPORTED_TEMPERATURES
            .iter()
            .any(|v| (temperature - v).abs() < TEMP_EPSILON)
    }

    fn validate_temperature(temperature: f64) -> anyhow::Result<()> {
        if !temperature.is_finite() {
            anyhow::bail!("KiloCLI model_provider received non-finite temperature value");
        }
        if !Self::supports_temperature(temperature) {
            anyhow::bail!(
                "temperature unsupported by KiloCLI: {temperature}. \
                 Supported values: 0.7 or 1.0"
            );
        }
        Ok(())
    }

    /// Invoke the kilo binary with the given prompt and optional model.
    /// Returns the trimmed stdout output as the assistant response.
    async fn invoke_cli(&self, message: &str, model: &str) -> anyhow::Result<String> {
        let mut cmd = Command::new(&self.binary_path);
        cmd.arg("--print");

        if Self::should_forward_model(model) {
            cmd.arg("--model").arg(model);
        }

        // Read prompt from stdin to avoid exposing sensitive content in process args.
        cmd.arg("-");
        cmd.kill_on_drop(true);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let child = cmd.spawn().map_err(|err| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "binary": self.binary_path.display().to_string(),
                        "phase": "spawn",
                        "error": format!("{}", err),
                    })),
                "kilocli: failed to spawn binary"
            );
            anyhow::Error::msg(format!(
                "Failed to spawn KiloCLI binary at {}: {err}. \
                 Ensure `kilo` is installed and in PATH, or set KILO_CLI_PATH.",
                self.binary_path.display()
            ))
        })?;

        crate::cli_output::run(child, message, "kilo_cli", KILO_CLI_REQUEST_TIMEOUT).await
    }
}

#[async_trait]
impl ModelProvider for KiloCliModelProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        if let Some(t) = temperature {
            Self::validate_temperature(t)?;
        }

        let full_message = match system_prompt {
            Some(system) if !system.is_empty() => {
                format!("{system}\n\n{message}")
            }
            _ => message.to_string(),
        };

        self.invoke_cli(&full_message, model).await
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        let text = self
            .chat_with_history(request.messages, model, temperature)
            .await?;

        Ok(ChatResponse {
            output_blocks: Vec::new(),
            text: Some(text),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        })
    }
}

impl ::zeroclaw_api::attribution::Attributable for KiloCliModelProvider {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Provider(
            ::zeroclaw_api::attribution::ProviderKind::Model(
                ::zeroclaw_api::attribution::ModelProviderKind::KiloCli,
            ),
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_explicit_binary_path() {
        let p = KiloCliModelProvider::builder("test")
            .binary_path(Some("/usr/local/bin/kilo"))
            .build();
        assert_eq!(p.binary_path, PathBuf::from("/usr/local/bin/kilo"));
    }

    #[test]
    fn new_defaults_to_kilo() {
        let p = KiloCliModelProvider::builder("test")
            .binary_path(None)
            .build();
        assert_eq!(p.binary_path, PathBuf::from("kilo"));
    }

    #[test]
    fn new_ignores_blank_binary_path() {
        let p = KiloCliModelProvider::builder("test")
            .binary_path(Some("   "))
            .build();
        assert_eq!(p.binary_path, PathBuf::from("kilo"));
    }

    #[test]
    fn should_forward_model_standard() {
        assert!(KiloCliModelProvider::should_forward_model("some-model"));
        assert!(KiloCliModelProvider::should_forward_model("gpt-4o"));
    }

    #[test]
    fn should_not_forward_default_model() {
        assert!(!KiloCliModelProvider::should_forward_model(
            DEFAULT_MODEL_MARKER
        ));
        assert!(!KiloCliModelProvider::should_forward_model(""));
        assert!(!KiloCliModelProvider::should_forward_model("   "));
    }

    #[test]
    fn validate_temperature_allows_defaults() {
        assert!(KiloCliModelProvider::validate_temperature(0.7).is_ok());
        assert!(KiloCliModelProvider::validate_temperature(1.0).is_ok());
    }

    #[test]
    fn validate_temperature_rejects_custom_value() {
        let err = KiloCliModelProvider::validate_temperature(0.2).unwrap_err();
        assert!(
            err.to_string()
                .contains("temperature unsupported by KiloCLI")
        );
    }

    #[tokio::test]
    async fn invoke_missing_binary_returns_error() {
        let model_provider = KiloCliModelProvider {
            alias: "test".to_string(),
            binary_path: PathBuf::from("/nonexistent/path/to/kilo"),
        };
        let result = model_provider.invoke_cli("hello", "default").await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Failed to spawn KiloCLI binary"),
            "unexpected error message: {msg}"
        );
    }
}
