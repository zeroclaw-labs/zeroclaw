//! Integration test: every `ProviderDispatch` method routes through
//! `attribution_span!(&*inner)`. Drives every wrapping method once and
//! asserts every captured `LogEvent` carries the inner provider's
//! `model_provider_type` / `model_provider_alias`.
//!
//! Locks in the contract that no future dispatcher addition can
//! silently lose attribution.

use std::sync::Arc;

use futures_util::StreamExt;
use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
use zeroclaw_api::model_provider::{
    ChatMessage, ChatRequest, ChatResponse, ModelInfo, ModelProvider, StreamChunk, StreamEvent,
    StreamOptions, StreamResult,
};
use zeroclaw_providers::ProviderDispatch;

struct EverythingFake {
    alias: String,
}

impl Attributable for EverythingFake {
    fn role(&self) -> Role {
        Role::Provider(ProviderKind::Model(ModelProviderKind::Anthropic))
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait::async_trait]
impl ModelProvider for EverythingFake {
    async fn chat(
        &self,
        _: ChatRequest<'_>,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        zeroclaw_log::record!(
            INFO,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
            "EverythingFake chat"
        );
        Ok(ChatResponse {
            text: Some(String::new()),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        })
    }

    async fn simple_chat(&self, _: &str, _: &str, _: Option<f64>) -> anyhow::Result<String> {
        zeroclaw_log::record!(
            INFO,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
            "EverythingFake simple_chat"
        );
        Ok(String::new())
    }

    async fn chat_with_system(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        zeroclaw_log::record!(
            INFO,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
            "EverythingFake chat_with_system"
        );
        Ok(String::new())
    }

    async fn chat_with_history(
        &self,
        _: &[ChatMessage],
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        zeroclaw_log::record!(
            INFO,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
            "EverythingFake chat_with_history"
        );
        Ok(String::new())
    }

    async fn chat_with_tools(
        &self,
        _: &[ChatMessage],
        _: &[serde_json::Value],
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        zeroclaw_log::record!(
            INFO,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
            "EverythingFake chat_with_tools"
        );
        Ok(ChatResponse {
            text: Some(String::new()),
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        })
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        zeroclaw_log::record!(
            INFO,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
            "EverythingFake list_models"
        );
        Ok(vec!["m".into()])
    }

    async fn list_models_with_pricing(&self) -> anyhow::Result<Vec<ModelInfo>> {
        zeroclaw_log::record!(
            INFO,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
            "EverythingFake list_models_with_pricing"
        );
        Ok(Vec::new())
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        zeroclaw_log::record!(
            INFO,
            zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
            "EverythingFake warmup"
        );
        Ok(())
    }

    fn stream_chat(
        &self,
        _: ChatRequest<'_>,
        _: &str,
        _: Option<f64>,
        _: StreamOptions,
    ) -> futures_util::stream::BoxStream<'static, StreamResult<StreamEvent>> {
        futures_util::stream::unfold(0u8, |state| async move {
            match state {
                0 => {
                    zeroclaw_log::record!(
                        INFO,
                        zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Note),
                        "EverythingFake stream_chat chunk"
                    );
                    Some((Ok(StreamEvent::TextDelta(StreamChunk::delta("x"))), 1u8))
                }
                1 => Some((Ok(StreamEvent::Final), 2u8)),
                _ => None,
            }
        })
        .boxed()
    }
}

#[tokio::test]
async fn every_dispatcher_method_attributes() {
    let _writer_guard = zeroclaw_log::__private_test_writer_lock();
    let _hook_guard = zeroclaw_log::__private_test_hook_lock();
    zeroclaw_log::try_install_capture_subscriber();
    let mut rx = zeroclaw_log::subscribe_or_install();
    while rx.try_recv().is_ok() {}

    let fake: Arc<dyn ModelProvider> = Arc::new(EverythingFake {
        alias: "integ".into(),
    });
    let d = ProviderDispatch::new(fake);
    let req = ChatRequest {
        messages: &[],
        tools: None,
        thinking: None,
    };

    d.chat(req, "m", None).await.unwrap();
    d.simple_chat("hi", "m", None).await.unwrap();
    d.chat_with_system(None, "hi", "m", None).await.unwrap();
    d.chat_with_history(&[], "m", None).await.unwrap();
    d.chat_with_tools(&[], &[], "m", None).await.unwrap();
    d.list_models().await.unwrap();
    d.list_models_with_pricing().await.unwrap();
    d.warmup().await.unwrap();
    let mut s = d.stream_chat(req, "m", None, StreamOptions::default());
    while s.next().await.is_some() {}

    let expected_messages = [
        "EverythingFake chat",
        "EverythingFake simple_chat",
        "EverythingFake chat_with_system",
        "EverythingFake chat_with_history",
        "EverythingFake chat_with_tools",
        "EverythingFake list_models",
        "EverythingFake list_models_with_pricing",
        "EverythingFake warmup",
        "EverythingFake stream_chat chunk",
    ];

    let mut seen: std::collections::HashSet<String> = Default::default();
    let mut all_received: Vec<String> = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while seen.len() < expected_messages.len() && std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let step = remaining.min(std::time::Duration::from_millis(50));
        match tokio::time::timeout(step, rx.recv()).await {
            Ok(Ok(value)) => {
                let Some(msg) = value.get("message").and_then(|v| v.as_str()) else {
                    continue;
                };
                all_received.push(msg.to_string());
                // Exact equality so e.g. "list_models" doesn't shadow
                // "list_models_with_pricing" via substring containment.
                let Some(expected) = expected_messages.iter().find(|e| msg == **e) else {
                    continue;
                };
                let zc = value.get("zeroclaw").expect("zeroclaw block present");
                assert_eq!(
                    zc.get("model_provider_type").and_then(|v| v.as_str()),
                    Some("anthropic"),
                    "message {msg:?} missing model_provider_type attribution; zc: {zc:?}",
                );
                assert_eq!(
                    zc.get("model_provider_alias").and_then(|v| v.as_str()),
                    Some("integ"),
                );
                seen.insert((*expected).to_string());
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
            Err(_elapsed) => {}
        }
    }
    let missing: Vec<_> = expected_messages
        .iter()
        .filter(|e| !seen.contains(**e))
        .collect();
    assert!(
        missing.is_empty(),
        "missed messages: {missing:?}; all received: {all_received:?}; matched: {seen:?}"
    );
    zeroclaw_log::clear_broadcast_hook();
}
