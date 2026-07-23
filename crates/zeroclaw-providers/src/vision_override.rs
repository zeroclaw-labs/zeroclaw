use super::ModelProvider;
use super::dispatch::ProviderDispatch;
use super::traits::{
    ChatMessage, ChatRequest, ChatResponse, StreamChunk, StreamEvent, StreamOptions, StreamResult,
};
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use zeroclaw_api::model_provider::ModelInfo;

/// Transparent decorator that overrides only a provider's vision (image input)
/// capability, delegating everything else to the wrapped provider.
///
/// Applied once at the provider-construction choke point
/// (`create_model_provider_inner`) when `[providers.models.<...>] vision` is
/// set, so every consumer of the resulting provider instance — the
/// vision-routing gate, the channel media pipeline, and the model router —
/// reads one consistent capability regardless of provider family. This avoids
/// having each family's factory (or each consumer) re-derive vision support.
pub struct VisionOverrideProvider {
    supports_vision: bool,
    inner: Box<dyn ModelProvider>,
}

impl VisionOverrideProvider {
    pub fn new(inner: Box<dyn ModelProvider>, supports_vision: bool) -> Self {
        Self {
            supports_vision,
            inner,
        }
    }
}

#[async_trait]
impl ModelProvider for VisionOverrideProvider {
    fn capabilities(&self) -> super::traits::ProviderCapabilities {
        // Patch the canonical `vision` capability; the default
        // `supports_vision()` reads this, and so does anything that inspects
        // capabilities directly, keeping the two consistent.
        let mut capabilities = self.inner.capabilities();
        capabilities.vision = self.supports_vision;
        capabilities
    }

    fn capabilities_for_model(&self, model: &str) -> super::traits::ProviderCapabilities {
        let mut capabilities = self.inner.capabilities_for_model(model);
        capabilities.vision = self.supports_vision;
        capabilities
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
        self.inner
            .stream_chat_with_system(system_prompt, message, model, temperature, options)
    }

    fn stream_chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> BoxStream<'static, StreamResult<StreamChunk>> {
        self.inner
            .stream_chat_with_history(messages, model, temperature, options)
    }

    fn stream_chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> BoxStream<'static, StreamResult<StreamEvent>> {
        ProviderDispatch::from_ref(&*self.inner).stream_chat(request, model, temperature, options)
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
    use crate::traits::ProviderCapabilities;
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
}
