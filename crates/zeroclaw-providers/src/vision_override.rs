use super::ModelProvider;
use super::dispatch::{
    ProviderDispatch, mark_current_dispatch_composite, stream_as_dispatch_composite,
};
use super::traits::{
    ChatMessage, ChatRequest, ChatResponse, StreamChunk, StreamEvent, StreamOptions, StreamResult,
};
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use zeroclaw_api::model_provider::ModelInfo;

/// Transparent decorator for factory-owned provider metadata.
///
/// At the provider-construction choke point it both applies an optional vision
/// override and marks the result as a known leaf with a stable dispatch
/// identity. Other callers can retain the original vision-only behavior via
/// [`VisionOverrideProvider::new`], which delegates identity stability.
pub struct VisionOverrideProvider {
    supports_vision: Option<bool>,
    stable_request_identity: Option<bool>,
    inner: Box<dyn ModelProvider>,
}

impl VisionOverrideProvider {
    pub fn new(inner: Box<dyn ModelProvider>, supports_vision: bool) -> Self {
        Self {
            supports_vision: Some(supports_vision),
            stable_request_identity: None,
            inner,
        }
    }

    pub(crate) fn factory_leaf(
        inner: Box<dyn ModelProvider>,
        supports_vision: Option<bool>,
    ) -> Self {
        Self {
            supports_vision,
            stable_request_identity: Some(true),
            inner,
        }
    }
}

#[async_trait]
impl ModelProvider for VisionOverrideProvider {
    fn has_stable_request_identity(&self, model: &str) -> bool {
        self.stable_request_identity
            .unwrap_or_else(|| self.inner.has_stable_request_identity(model))
    }

    fn capabilities(&self) -> super::traits::ProviderCapabilities {
        // Patch the canonical `vision` capability; the default
        // `supports_vision()` reads this, and so does anything that inspects
        // capabilities directly, keeping the two consistent.
        let mut capabilities = self.inner.capabilities();
        if let Some(supports_vision) = self.supports_vision {
            capabilities.vision = supports_vision;
        }
        capabilities
    }

    fn capabilities_for_model(&self, model: &str) -> super::traits::ProviderCapabilities {
        let mut capabilities = self.inner.capabilities_for_model(model);
        if let Some(supports_vision) = self.supports_vision {
            capabilities.vision = supports_vision;
        }
        capabilities
    }

    fn has_mixed_native_tool_support_for_model(&self, model: &str) -> bool {
        self.inner.has_mixed_native_tool_support_for_model(model)
    }

    fn default_temperature(&self) -> f64 {
        self.inner.default_temperature()
    }

    fn default_max_tokens(&self) -> u32 {
        self.inner.default_max_tokens()
    }

    fn default_timeout_secs(&self) -> u64 {
        self.inner.default_timeout_secs()
    }

    fn default_base_url(&self) -> Option<&str> {
        self.inner.default_base_url()
    }

    fn default_wire_api(&self) -> &str {
        self.inner.default_wire_api()
    }

    fn convert_tools(&self, tools: &[zeroclaw_api::tool::ToolSpec]) -> super::traits::ToolsPayload {
        self.inner.convert_tools(tools)
    }

    fn supports_native_tools(&self) -> bool {
        self.inner.supports_native_tools()
    }

    fn supports_vision(&self) -> bool {
        self.supports_vision
            .unwrap_or_else(|| self.inner.supports_vision())
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    fn supports_streaming_tool_events(&self) -> bool {
        self.inner.supports_streaming_tool_events()
    }

    async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        ProviderDispatch::from_ref(&*self.inner).list_models().await
    }

    async fn list_models_with_pricing(&self) -> anyhow::Result<Vec<ModelInfo>> {
        // Must be delegated: the trait default rebuilds `ModelInfo` with
        // `pricing: None`, which would silently strip live pricing metadata
        // from any wrapped provider whose listing carries it.
        ProviderDispatch::from_ref(&*self.inner)
            .list_models_with_pricing()
            .await
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        ProviderDispatch::from_ref(&*self.inner).warmup().await
    }

    async fn simple_chat(
        &self,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        mark_current_dispatch_composite();
        ProviderDispatch::from_ref(&*self.inner)
            .simple_chat(message, model, temperature)
            .await
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        mark_current_dispatch_composite();
        ProviderDispatch::from_ref(&*self.inner)
            .chat_with_system(system_prompt, message, model, temperature)
            .await
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        mark_current_dispatch_composite();
        ProviderDispatch::from_ref(&*self.inner)
            .chat_with_history(messages, model, temperature)
            .await
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        mark_current_dispatch_composite();
        ProviderDispatch::from_ref(&*self.inner)
            .chat(request, model, temperature)
            .await
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        mark_current_dispatch_composite();
        ProviderDispatch::from_ref(&*self.inner)
            .chat_with_tools(messages, tools, model, temperature)
            .await
    }

    fn stream_chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> BoxStream<'static, StreamResult<StreamChunk>> {
        stream_as_dispatch_composite(
            ProviderDispatch::from_ref(&*self.inner).stream_chat_with_system(
                system_prompt,
                message,
                model,
                temperature,
                options,
            ),
        )
    }

    fn stream_chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> BoxStream<'static, StreamResult<StreamChunk>> {
        stream_as_dispatch_composite(
            ProviderDispatch::from_ref(&*self.inner).stream_chat_with_history(
                messages,
                model,
                temperature,
                options,
            ),
        )
    }

    fn stream_chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> BoxStream<'static, StreamResult<StreamEvent>> {
        stream_as_dispatch_composite(ProviderDispatch::from_ref(&*self.inner).stream_chat(
            request,
            model,
            temperature,
            options,
        ))
    }
}

impl zeroclaw_api::attribution::Attributable for VisionOverrideProvider {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        self.inner.role()
    }
    fn alias(&self) -> &str {
        self.inner.alias()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::AccountedChatScope;
    use crate::traits::ProviderCapabilities;
    use std::sync::Arc;
    use zeroclaw_api::attribution::{Attributable, ModelProviderKind, ProviderKind, Role};
    use zeroclaw_api::model_provider::ModelPricing;

    /// Minimal inner provider that reports vision support and returns a priced
    /// model listing. Everything else falls back to the trait defaults.
    struct PricedVisionFake;

    impl Attributable for PricedVisionFake {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Anthropic))
        }
        fn alias(&self) -> &str {
            "priced_vision_fake"
        }
    }

    #[async_trait]
    impl ModelProvider for PricedVisionFake {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                vision: true,
                ..ProviderCapabilities::default()
            }
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            unreachable!("chat is not exercised by the vision-override pricing test")
        }

        async fn list_models_with_pricing(&self) -> anyhow::Result<Vec<ModelInfo>> {
            let pricing: ModelPricing = serde_json::from_str(r#"{"prompt":"0.000005"}"#).unwrap();
            Ok(vec![ModelInfo {
                id: "priced-model".to_string(),
                pricing: Some(pricing),
                context_window: None,
            }])
        }
    }

    #[tokio::test]
    async fn vision_override_flips_capability_without_stripping_pricing() {
        let wrapped = VisionOverrideProvider::new(Box::new(PricedVisionFake), false);

        // The vision capability is overridden on both surfaces ...
        assert!(!wrapped.supports_vision());
        assert!(!wrapped.capabilities().vision);

        // ... while other delegated metadata is untouched: the inner listing's
        // live pricing survives (the trait default would rebuild the entries
        // with pricing: None).
        let models = wrapped.list_models_with_pricing().await.unwrap();
        assert_eq!(models.len(), 1);
        assert!(
            models[0].pricing.is_some(),
            "vision override must delegate list_models_with_pricing and keep pricing"
        );
    }

    struct ModelAwareCapabilityFake;

    impl Attributable for ModelAwareCapabilityFake {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "model_aware_capability_fake"
        }
    }

    #[async_trait]
    impl ModelProvider for ModelAwareCapabilityFake {
        fn capabilities_for_model(&self, model: &str) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: model == "routed-model",
                vision: true,
                ..ProviderCapabilities::default()
            }
        }

        fn has_mixed_native_tool_support_for_model(&self, model: &str) -> bool {
            model == "routed-model"
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }
    }

    #[test]
    fn vision_override_preserves_model_aware_tool_capabilities() {
        let wrapped = VisionOverrideProvider::new(Box::new(ModelAwareCapabilityFake), false);

        let capabilities = wrapped.capabilities_for_model("routed-model");
        assert!(
            capabilities.native_tool_calling,
            "the vision decorator must preserve model-aware native-tool support"
        );
        assert!(
            !capabilities.vision,
            "the configured vision override remains authoritative"
        );
        assert!(
            wrapped.has_mixed_native_tool_support_for_model("routed-model"),
            "the vision decorator must transparently forward mixed-chain detection"
        );
    }

    #[test]
    fn request_identity_defaults_unstable_and_arc_delegates() {
        let unknown = PricedVisionFake;
        assert!(!unknown.has_stable_request_identity("model"));

        let stable = Arc::new(VisionOverrideProvider::factory_leaf(
            Box::new(PricedVisionFake),
            None,
        ));
        assert!(stable.has_stable_request_identity("model"));
    }

    #[test]
    fn ordinary_vision_override_preserves_inner_request_identity() {
        let unstable = VisionOverrideProvider::new(Box::new(PricedVisionFake), false);
        assert!(!unstable.has_stable_request_identity("model"));

        let stable_inner = VisionOverrideProvider::factory_leaf(Box::new(PricedVisionFake), None);
        let stable = VisionOverrideProvider::new(Box::new(stable_inner), false);
        assert!(stable.has_stable_request_identity("model"));
    }

    struct AccountedVisionLeaf;

    impl Attributable for AccountedVisionLeaf {
        fn role(&self) -> Role {
            Role::Provider(ProviderKind::Model(ModelProviderKind::Custom))
        }

        fn alias(&self) -> &str {
            "configured.inner"
        }
    }

    #[async_trait]
    impl ModelProvider for AccountedVisionLeaf {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some("ok".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    #[tokio::test]
    async fn accounting_through_vision_override_keeps_inner_identity() {
        let wrapped = VisionOverrideProvider::new(Box::new(AccountedVisionLeaf), false);
        let messages = vec![ChatMessage::user("hello")];
        let outcome = ProviderDispatch::from_ref(&wrapped)
            .chat_accounted_outcome(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "served-model",
                None,
            )
            .await;

        assert!(outcome.result.is_ok());
        assert_eq!(outcome.accounting.attempts().len(), 1);
        let leaf = &outcome.accounting.attempts()[0];
        assert_eq!(
            (leaf.provider_ref(), leaf.model()),
            ("configured.inner", "served-model")
        );
    }

    #[tokio::test]
    async fn history_accounting_through_vision_override_keeps_only_inner_leaf() {
        let wrapped = VisionOverrideProvider::new(Box::new(AccountedVisionLeaf), false);
        let messages = vec![ChatMessage::user("hello")];
        let scope = AccountedChatScope::new();

        let result = scope
            .scope(ProviderDispatch::from_ref(&wrapped).chat_with_history(
                &messages,
                "served-model",
                None,
            ))
            .await;

        assert!(result.is_ok());
        let report = scope.take();
        assert_eq!(report.attempts().len(), 1);
        let leaf = &report.attempts()[0];
        assert_eq!(
            (leaf.provider_ref(), leaf.model()),
            ("configured.inner", "served-model")
        );
    }
}
