//! `ProviderDispatch` — single source of truth for `attribution_span!`
//! on the [`ModelProvider`] surface.
//!
//! Every direct call to a `ModelProvider` method in the workspace goes
//! through this helper so the resulting `LogEvent` carries the inner
//! provider's alias-bound attribution without the call site naming any
//! of it. Each wrapping method opens
//! `attribution_span!(&*self.inner)` (and, for the methods that take a
//! model string, an additional `scope!(model: …)`) around the
//! underlying call.
//!
//! Adding a new `ModelProvider` method: add the wrapping method here
//! and extend the `scripts/ci/rust_quality_gate.sh` grep gate's
//! protected method list. The dispatch module is the only place in the
//! workspace that opens `attribution_span!` for a `ModelProvider`.

use std::sync::Arc;

use zeroclaw_api::model_provider::ModelProvider;

/// Wraps a model provider so every call opens the correct
/// `attribution_span!` automatically. See the module docs for the
/// rationale and the CI gate that enforces routing through this type.
pub struct ProviderDispatch {
    inner: Arc<dyn ModelProvider>,
}

impl ProviderDispatch {
    /// Wrap an `Arc<dyn ModelProvider>` so its method calls open
    /// `attribution_span!(&*inner)` automatically.
    #[must_use]
    pub fn new(inner: Arc<dyn ModelProvider>) -> Self {
        Self { inner }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use zeroclaw_api::model_provider::{ChatRequest, ChatResponse, ModelProvider};

    struct FakeAnthropic {
        alias: String,
    }

    impl Attributable for FakeAnthropic {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Anthropic))
        }
        fn alias(&self) -> &str {
            &self.alias
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for FakeAnthropic {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            zeroclaw_log::record!(
                INFO,
                zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
                "fake-anthropic chat called"
            );
            Ok(ChatResponse {
                text: Some(String::new()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    #[tokio::test]
    async fn dispatch_chat_attaches_inner_provider_attribution() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let fake: Arc<dyn ModelProvider> = Arc::new(FakeAnthropic {
            alias: "test-alias".into(),
        });
        let dispatch = ProviderDispatch::new(fake);
        let request = ChatRequest {
            messages: &[],
            tools: None,
            thinking: None,
        };
        let _ = dispatch.chat(request, "claude-sonnet-4-6", None).await;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut found = false;
        while !found && std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let step = remaining.min(std::time::Duration::from_millis(50));
            match tokio::time::timeout(step, rx.recv()).await {
                Ok(Ok(value)) => {
                    if value
                        .get("message")
                        .and_then(|v| v.as_str())
                        .map(|s| s.contains("fake-anthropic chat called"))
                        .unwrap_or(false)
                    {
                        let zc = value.get("zeroclaw").expect("zeroclaw block present");
                        assert_eq!(
                            zc.get("model_provider").and_then(|v| v.as_str()),
                            Some("anthropic.test-alias"),
                            "expected composite model_provider; got: {zc:?}"
                        );
                        assert_eq!(
                            zc.get("model_provider_type").and_then(|v| v.as_str()),
                            Some("anthropic"),
                        );
                        assert_eq!(
                            zc.get("model_provider_alias").and_then(|v| v.as_str()),
                            Some("test-alias"),
                        );
                        assert_eq!(
                            zc.get("model").and_then(|v| v.as_str()),
                            Some("claude-sonnet-4-6"),
                        );
                        found = true;
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_elapsed) => {}
            }
        }
        assert!(found, "did not capture the fake-anthropic event");
        zeroclaw_log::clear_broadcast_hook();
    }
}
