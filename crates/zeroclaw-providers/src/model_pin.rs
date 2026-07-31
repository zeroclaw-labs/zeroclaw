use super::ModelProvider;
use super::dispatch::ProviderDispatch;
use super::traits::{
    ChatMessage, ChatRequest, ChatResponse, StreamChunk, StreamEvent, StreamOptions, StreamResult,
};
use async_trait::async_trait;
use futures_util::stream::BoxStream;

pub struct ModelPinnedProvider {
    alias: String,
    pinned_model: String,
    inner: Box<dyn ModelProvider>,
}

/// Typed builder for [`ModelPinnedProvider`].
///
/// `alias` is the only positional argument. Both `pinned_model` and
/// `inner` are semantically required at `build()` time; they moved off
/// `new(...)` because two adjacent `&str` positional args (`alias` and
/// `pinned_model`) had a real swap-risk surface — silently pinning the
/// wrong provider to the wrong model.
#[must_use]
pub struct ModelPinnedProviderBuilder {
    alias: String,
    pinned_model: Option<String>,
    inner: Option<Box<dyn ModelProvider>>,
}

impl ModelPinnedProviderBuilder {
    /// The model ID every request to this provider is rewritten to.
    /// Required.
    pub fn pinned_model(mut self, model: &str) -> Self {
        self.pinned_model = Some(model.to_string());
        self
    }

    /// The inner provider whose model this pin overrides. Required.
    pub fn inner(mut self, inner: Box<dyn ModelProvider>) -> Self {
        self.inner = Some(inner);
        self
    }

    /// # Panics
    /// Panics if [`Self::pinned_model`] or [`Self::inner`] was not
    /// called — neither has a sensible default.
    pub fn build(self) -> ModelPinnedProvider {
        ModelPinnedProvider {
            alias: self.alias,
            pinned_model: self
                .pinned_model
                .expect("ModelPinnedProviderBuilder: pinned_model() is required"),
            inner: self
                .inner
                .expect("ModelPinnedProviderBuilder: inner() is required"),
        }
    }
}

impl ModelPinnedProvider {
    /// Entry point. Only `alias` is taken positionally; the required
    /// `pinned_model` and `inner` provider both go through labelled
    /// chain methods so call sites cannot silently swap them.
    pub fn builder(alias: &str) -> ModelPinnedProviderBuilder {
        ModelPinnedProviderBuilder {
            alias: alias.to_string(),
            pinned_model: None,
            inner: None,
        }
    }

    pub(crate) fn pinned_model(&self) -> &str {
        &self.pinned_model
    }
}

#[async_trait]
impl ModelProvider for ModelPinnedProvider {
    fn capabilities(&self) -> super::traits::ProviderCapabilities {
        self.inner.capabilities()
    }

    fn capabilities_for_model(&self, _model: &str) -> super::traits::ProviderCapabilities {
        self.inner.capabilities_for_model(&self.pinned_model)
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
        self.inner.supports_vision()
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

    async fn warmup(&self) -> anyhow::Result<()> {
        ProviderDispatch::from_ref(&*self.inner).warmup().await
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        _model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        ProviderDispatch::from_ref(&*self.inner)
            .chat_with_system(system_prompt, message, &self.pinned_model, temperature)
            .await
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        _model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        ProviderDispatch::from_ref(&*self.inner)
            .chat_with_history(messages, &self.pinned_model, temperature)
            .await
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        ProviderDispatch::from_ref(&*self.inner)
            .chat(request, &self.pinned_model, temperature)
            .await
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        _model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        ProviderDispatch::from_ref(&*self.inner)
            .chat_with_tools(messages, tools, &self.pinned_model, temperature)
            .await
    }

    fn stream_chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        _model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> BoxStream<'static, StreamResult<StreamChunk>> {
        // stream_chat_with_system is not on ProviderDispatch's protected
        // surface — the dispatcher only wraps stream_chat. Pass through.
        self.inner.stream_chat_with_system(
            system_prompt,
            message,
            &self.pinned_model,
            temperature,
            options,
        )
    }

    fn stream_chat_with_history(
        &self,
        messages: &[ChatMessage],
        _model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> BoxStream<'static, StreamResult<StreamChunk>> {
        // Same passthrough rationale as stream_chat_with_system.
        self.inner
            .stream_chat_with_history(messages, &self.pinned_model, temperature, options)
    }

    fn stream_chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> BoxStream<'static, StreamResult<StreamEvent>> {
        ProviderDispatch::from_ref(&*self.inner).stream_chat(
            request,
            &self.pinned_model,
            temperature,
            options,
        )
    }
}

impl zeroclaw_api::attribution::Attributable for ModelPinnedProvider {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        self.inner.role()
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}
