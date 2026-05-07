use super::ModelProvider;
use super::traits::{
    ChatMessage, ChatRequest, ChatResponse, StreamChunk, StreamEvent, StreamOptions, StreamResult,
};
use async_trait::async_trait;
use futures_util::stream::BoxStream;
use std::collections::HashMap;

/// Score a model against a user-keyed pricing map. Sums any entry matching
/// the model directly, plus optional `.input` and `.output` dimension keys.
/// Returns `None` when nothing matches.
fn score_model(pricing: &HashMap<String, f64>, model: &str) -> Option<f64> {
    let mut total = 0.0;
    let mut matched = false;
    if let Some(v) = pricing.get(model) {
        total += *v;
        matched = true;
    }
    if let Some(v) = pricing.get(&format!("{model}.input")) {
        total += *v;
        matched = true;
    }
    if let Some(v) = pricing.get(&format!("{model}.output")) {
        total += *v;
        matched = true;
    }
    matched.then_some(total)
}

/// A single route: maps a task hint to a model_provider + model combo.
#[derive(Debug, Clone)]
pub struct Route {
    pub provider_name: String,
    pub model: String,
}

/// Multi-model router — routes requests to different model_provider+model combos
/// based on a task hint encoded in the model parameter.
///
/// The model parameter can be:
/// - A regular model name (e.g. "anthropic/claude-sonnet-4") → uses default model_provider
/// - A hint-prefixed string (e.g. "hint:reasoning") → resolves via route table
///
/// This wraps multiple pre-created model_providers and selects the right one per request.
pub struct RouterModelProvider {
    routes: HashMap<String, (usize, String)>, // hint → (provider_index, model)
    model_providers: Vec<(String, Box<dyn ModelProvider>)>,
    default_index: usize,
    default_model: String,
}

impl RouterModelProvider {
    /// Create a new router with a default model_provider and optional routes.
    ///
    /// `model_providers` is a list of (name, model_provider) pairs. The first one is the default.
    /// `routes` maps hint names to Route structs containing provider_name and model.
    pub fn new(
        model_providers: Vec<(String, Box<dyn ModelProvider>)>,
        routes: Vec<(String, Route)>,
        default_model: String,
    ) -> Self {
        // Build model_provider name → index lookup
        let name_to_index: HashMap<&str, usize> = model_providers
            .iter()
            .enumerate()
            .map(|(i, (name, _))| (name.as_str(), i))
            .collect();

        // Resolve routes to model_provider indices
        let resolved_routes: HashMap<String, (usize, String)> = routes
            .into_iter()
            .filter_map(|(hint, route)| {
                let index = name_to_index.get(route.provider_name.as_str()).copied();
                match index {
                    Some(i) => Some((hint, (i, route.model))),
                    None => {
                        tracing::warn!(
                            hint = hint,
                            model_provider = route.provider_name,
                            "Route references unknown model_provider, skipping"
                        );
                        None
                    }
                }
            })
            .collect();

        Self {
            routes: resolved_routes,
            model_providers,
            default_index: 0,
            default_model,
        }
    }

    /// Resolve a model parameter to the cheapest qualifying route based on pricing.
    ///
    /// If the model starts with `"hint:cost-optimized"` or `"hint:cheapest"`, this
    /// method scores each route by `input_price + output_price` (a simple proxy for
    /// total cost), optionally filtering by capability requirements, and returns the
    /// cheapest qualifying route.
    ///
    /// Falls back to the default route when no pricing data matches.
    pub fn resolve_cost_optimized(
        &self,
        model: &str,
        model_provider_pricing: &HashMap<String, HashMap<String, f64>>,
        required_vision: bool,
        required_tools: bool,
    ) -> (usize, String) {
        let hint = model.strip_prefix("hint:");
        let is_cost_hint = matches!(hint, Some("cost-optimized" | "cheapest"));

        if !is_cost_hint {
            return self.resolve(model);
        }

        let mut candidates: Vec<(usize, String, f64)> = Vec::new();

        for (idx, route_model) in self.routes.values() {
            // Capability filtering
            if let Some((_, model_provider)) = self.model_providers.get(*idx) {
                if required_vision && !model_provider.supports_vision() {
                    continue;
                }
                if required_tools && !model_provider.supports_native_tools() {
                    continue;
                }
            }

            let Some((model_provider_name, _)) = self.model_providers.get(*idx) else {
                continue;
            };
            if let Some(pricing) = model_provider_pricing.get(model_provider_name)
                && let Some(total_cost) = score_model(pricing, route_model)
            {
                candidates.push((*idx, route_model.clone(), total_cost));
            }
        }

        // Sort by total cost (ascending) and pick the cheapest
        candidates.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));

        if let Some((idx, route_model, _)) = candidates.into_iter().next() {
            return (idx, route_model);
        }

        // Fallback to default
        tracing::warn!(
            "No cost-optimized route found with matching pricing data, \
             falling back to default"
        );
        (self.default_index, self.default_model.clone())
    }

    /// Resolve a model parameter to a (model_provider, actual_model) pair.
    ///
    /// If the model starts with "hint:", look up the hint in the route table.
    /// Otherwise, use the default model_provider with the given model name.
    /// Resolve a model parameter to a (provider_index, actual_model) pair.
    fn resolve(&self, model: &str) -> (usize, String) {
        if let Some(hint) = model.strip_prefix("hint:") {
            if let Some((idx, resolved_model)) = self.routes.get(hint) {
                return (*idx, resolved_model.clone());
            }
            tracing::warn!(
                hint = hint,
                "Unknown route hint, falling back to default model_provider"
            );
        }

        // Not a hint or hint not found — use default model_provider with the model as-is
        (self.default_index, model.to_string())
    }
}

/// A cost-optimized routing strategy that selects the cheapest qualifying
/// model_provider from the route table based on per-provider pricing maps.
///
/// Pricing is keyed by model_provider name (the alias under
/// `[providers.models.<model_provider>.<alias>]`); each model_provider's pricing map
/// holds user-defined keys (model identifiers, optionally suffixed with
/// `.input` / `.output`) mapped to USD-per-1M-token rates.
#[derive(Debug, Clone)]
pub struct CostOptimizedStrategy {
    /// Per-provider pricing data (model_provider name → user-keyed pricing map).
    pub model_provider_pricing: HashMap<String, HashMap<String, f64>>,
    /// Whether the request requires vision support.
    pub required_vision: bool,
    /// Whether the request requires native tool support.
    pub required_tools: bool,
}

impl CostOptimizedStrategy {
    /// Create a new cost-optimized strategy with the given per-provider
    /// pricing data.
    pub fn new(model_provider_pricing: HashMap<String, HashMap<String, f64>>) -> Self {
        Self {
            model_provider_pricing,
            required_vision: false,
            required_tools: false,
        }
    }

    /// Set whether vision support is required.
    pub fn with_vision(mut self, required: bool) -> Self {
        self.required_vision = required;
        self
    }

    /// Set whether native tool support is required.
    pub fn with_tools(mut self, required: bool) -> Self {
        self.required_tools = required;
        self
    }

    /// Score a route by summing pricing entries that match the model.
    /// Returns `None` if no pricing data is available for the route.
    pub fn score(&self, model_provider_name: &str, model: &str) -> Option<f64> {
        let pricing = self.model_provider_pricing.get(model_provider_name)?;
        score_model(pricing, model)
    }
}

#[async_trait]
impl ModelProvider for RouterModelProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let (provider_idx, resolved_model) = self.resolve(model);

        let (provider_name, model_provider) = &self.model_providers[provider_idx];
        tracing::info!(
            model_provider = provider_name.as_str(),
            model = resolved_model.as_str(),
            "Router dispatching request"
        );

        model_provider
            .chat_with_system(system_prompt, message, &resolved_model, temperature)
            .await
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        let (provider_idx, resolved_model) = self.resolve(model);
        let (_, model_provider) = &self.model_providers[provider_idx];
        model_provider
            .chat_with_history(messages, &resolved_model, temperature)
            .await
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        let (provider_idx, resolved_model) = self.resolve(model);
        let (_, model_provider) = &self.model_providers[provider_idx];
        model_provider
            .chat(request, &resolved_model, temperature)
            .await
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: Option<f64>,
    ) -> anyhow::Result<ChatResponse> {
        let (provider_idx, resolved_model) = self.resolve(model);
        let (_, model_provider) = &self.model_providers[provider_idx];
        model_provider
            .chat_with_tools(messages, tools, &resolved_model, temperature)
            .await
    }

    fn supports_native_tools(&self) -> bool {
        self.model_providers
            .get(self.default_index)
            .map(|(_, p)| p.supports_native_tools())
            .unwrap_or(false)
    }

    fn supports_streaming(&self) -> bool {
        self.model_providers
            .iter()
            .any(|(_, model_provider)| model_provider.supports_streaming())
    }

    fn supports_streaming_tool_events(&self) -> bool {
        self.model_providers
            .iter()
            .any(|(_, model_provider)| model_provider.supports_streaming_tool_events())
    }

    fn stream_chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> BoxStream<'static, StreamResult<StreamChunk>> {
        let (provider_idx, resolved_model) = self.resolve(model);
        let (_, model_provider) = &self.model_providers[provider_idx];
        model_provider.stream_chat_with_history(messages, &resolved_model, temperature, options)
    }

    fn stream_chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: Option<f64>,
        options: StreamOptions,
    ) -> BoxStream<'static, StreamResult<StreamEvent>> {
        let (provider_idx, resolved_model) = self.resolve(model);
        let (_, model_provider) = &self.model_providers[provider_idx];
        model_provider.stream_chat(request, &resolved_model, temperature, options)
    }

    fn supports_vision(&self) -> bool {
        self.model_providers
            .iter()
            .any(|(_, model_provider)| model_provider.supports_vision())
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        for (name, model_provider) in &self.model_providers {
            tracing::info!(model_provider = name, "Warming up routed model_provider");
            if let Err(e) = model_provider.warmup().await {
                tracing::warn!(model_provider = name, "Warmup failed (non-fatal): {e}");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zeroclaw_api::tool::ToolSpec;

    struct MockModelProvider {
        calls: Arc<AtomicUsize>,
        response: &'static str,
        last_model: parking_lot::Mutex<String>,
    }

    impl MockModelProvider {
        fn new(response: &'static str) -> Self {
            Self {
                calls: Arc::new(AtomicUsize::new(0)),
                response,
                last_model: parking_lot::Mutex::new(String::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn last_model(&self) -> String {
            self.last_model.lock().clone()
        }
    }

    #[async_trait]
    impl ModelProvider for MockModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_model.lock() = model.to_string();
            Ok(self.response.to_string())
        }
    }

    fn make_router(
        model_providers: Vec<(&'static str, &'static str)>,
        routes: Vec<(&str, &str, &str)>,
    ) -> (RouterModelProvider, Vec<Arc<MockModelProvider>>) {
        let mocks: Vec<Arc<MockModelProvider>> = model_providers
            .iter()
            .map(|(_, response)| Arc::new(MockModelProvider::new(response)))
            .collect();

        let provider_list: Vec<(String, Box<dyn ModelProvider>)> = model_providers
            .iter()
            .zip(mocks.iter())
            .map(|((name, _), mock)| {
                (
                    (*name).to_string(),
                    Box::new(Arc::clone(mock)) as Box<dyn ModelProvider>,
                )
            })
            .collect();

        let route_list: Vec<(String, Route)> = routes
            .iter()
            .map(|(hint, provider_name, model)| {
                (
                    (*hint).to_string(),
                    Route {
                        provider_name: (*provider_name).to_string(),
                        model: (*model).to_string(),
                    },
                )
            })
            .collect();

        let router =
            RouterModelProvider::new(provider_list, route_list, "default-model".to_string());

        (router, mocks)
    }

    // Arc<MockModelProvider> ModelProvider impl provided by blanket impl in zeroclaw-types.

    struct StreamingMockModelProvider {
        stream_calls: Arc<AtomicUsize>,
        last_stream_model: parking_lot::Mutex<String>,
        response: &'static str,
    }

    impl StreamingMockModelProvider {
        fn new(response: &'static str) -> Self {
            Self {
                stream_calls: Arc::new(AtomicUsize::new(0)),
                last_stream_model: parking_lot::Mutex::new(String::new()),
                response,
            }
        }
    }

    #[async_trait]
    impl ModelProvider for StreamingMockModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat_with_history(
            &self,
            _messages: &[ChatMessage],
            model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> BoxStream<'static, StreamResult<StreamChunk>> {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            *self.last_stream_model.lock() = model.to_string();
            let chunks = vec![
                Ok(StreamChunk::delta(self.response)),
                Ok(StreamChunk::final_chunk()),
            ];
            futures_util::stream::iter(chunks).boxed()
        }
    }

    // Arc<StreamingMockModelProvider> ModelProvider impl provided by blanket impl in zeroclaw-types.

    struct ToolEventStreamingMockModelProvider {
        stream_calls: Arc<AtomicUsize>,
        tool_event_calls: Arc<AtomicUsize>,
        last_stream_model: parking_lot::Mutex<String>,
    }

    impl ToolEventStreamingMockModelProvider {
        fn new() -> Self {
            Self {
                stream_calls: Arc::new(AtomicUsize::new(0)),
                tool_event_calls: Arc::new(AtomicUsize::new(0)),
                last_stream_model: parking_lot::Mutex::new(String::new()),
            }
        }
    }

    #[async_trait]
    impl ModelProvider for ToolEventStreamingMockModelProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok("ok".to_string())
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn supports_streaming_tool_events(&self) -> bool {
            true
        }

        fn stream_chat(
            &self,
            request: ChatRequest<'_>,
            model: &str,
            _temperature: Option<f64>,
            _options: StreamOptions,
        ) -> BoxStream<'static, StreamResult<StreamEvent>> {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            if request.tools.is_some_and(|tools| !tools.is_empty()) {
                self.tool_event_calls.fetch_add(1, Ordering::SeqCst);
            }
            *self.last_stream_model.lock() = model.to_string();
            futures_util::stream::iter(vec![
                Ok(StreamEvent::ToolCall(crate::traits::ToolCall {
                    id: "call_router_1".to_string(),
                    name: "shell".to_string(),
                    arguments: r#"{"command":"date"}"#.to_string(),
                    extra_content: None,
                })),
                Ok(StreamEvent::Final),
            ])
            .boxed()
        }
    }

    // Arc<ToolEventStreamingMockModelProvider> ModelProvider impl provided by blanket impl in zeroclaw-types.

    #[tokio::test]
    async fn routes_hint_to_correct_provider() {
        let (router, mocks) = make_router(
            vec![("fast", "fast-response"), ("smart", "smart-response")],
            vec![
                ("fast", "fast", "llama-3-70b"),
                ("reasoning", "smart", "claude-opus"),
            ],
        );

        let result = router
            .simple_chat("hello", "hint:reasoning", Some(0.5))
            .await
            .unwrap();
        assert_eq!(result, "smart-response");
        assert_eq!(mocks[1].call_count(), 1);
        assert_eq!(mocks[1].last_model(), "claude-opus");
        assert_eq!(mocks[0].call_count(), 0);
    }

    #[tokio::test]
    async fn routes_fast_hint() {
        let (router, mocks) = make_router(
            vec![("fast", "fast-response"), ("smart", "smart-response")],
            vec![("fast", "fast", "llama-3-70b")],
        );

        let result = router
            .simple_chat("hello", "hint:fast", Some(0.5))
            .await
            .unwrap();
        assert_eq!(result, "fast-response");
        assert_eq!(mocks[0].call_count(), 1);
        assert_eq!(mocks[0].last_model(), "llama-3-70b");
    }

    #[tokio::test]
    async fn unknown_hint_falls_back_to_default() {
        let (router, mocks) = make_router(
            vec![("default", "default-response"), ("other", "other-response")],
            vec![],
        );

        let result = router
            .simple_chat("hello", "hint:nonexistent", Some(0.5))
            .await
            .unwrap();
        assert_eq!(result, "default-response");
        assert_eq!(mocks[0].call_count(), 1);
        // Falls back to default with the hint as model name
        assert_eq!(mocks[0].last_model(), "hint:nonexistent");
    }

    #[tokio::test]
    async fn non_hint_model_uses_default_provider() {
        let (router, mocks) = make_router(
            vec![
                ("primary", "primary-response"),
                ("secondary", "secondary-response"),
            ],
            vec![("code", "secondary", "codellama")],
        );

        let result = router
            .simple_chat("hello", "anthropic/claude-sonnet-4-20250514", Some(0.5))
            .await
            .unwrap();
        assert_eq!(result, "primary-response");
        assert_eq!(mocks[0].call_count(), 1);
        assert_eq!(mocks[0].last_model(), "anthropic/claude-sonnet-4-20250514");
    }

    #[test]
    fn resolve_preserves_model_for_non_hints() {
        let (router, _) = make_router(vec![("default", "ok")], vec![]);

        let (idx, model) = router.resolve("gpt-4o");
        assert_eq!(idx, 0);
        assert_eq!(model, "gpt-4o");
    }

    #[test]
    fn resolve_strips_hint_prefix() {
        let (router, _) = make_router(
            vec![("fast", "ok"), ("smart", "ok")],
            vec![("reasoning", "smart", "claude-opus")],
        );

        let (idx, model) = router.resolve("hint:reasoning");
        assert_eq!(idx, 1);
        assert_eq!(model, "claude-opus");
    }

    #[test]
    fn skips_routes_with_unknown_provider() {
        let (router, _) = make_router(
            vec![("default", "ok")],
            vec![("broken", "nonexistent", "model")],
        );

        // Route should not exist
        assert!(!router.routes.contains_key("broken"));
    }

    #[tokio::test]
    async fn warmup_calls_all_providers() {
        let (router, _) = make_router(vec![("a", "ok"), ("b", "ok")], vec![]);

        // Warmup should not error
        assert!(router.warmup().await.is_ok());
    }

    #[tokio::test]
    async fn chat_with_system_passes_system_prompt() {
        let mock = Arc::new(MockModelProvider::new("response"));
        let router = RouterModelProvider::new(
            vec![(
                "default".into(),
                Box::new(Arc::clone(&mock)) as Box<dyn ModelProvider>,
            )],
            vec![],
            "model".into(),
        );

        let result = router
            .chat_with_system(Some("system"), "hello", "model", Some(0.5))
            .await
            .unwrap();
        assert_eq!(result, "response");
        assert_eq!(mock.call_count(), 1);
    }

    #[tokio::test]
    async fn chat_with_tools_delegates_to_resolved_provider() {
        let mock = Arc::new(MockModelProvider::new("tool-response"));
        let router = RouterModelProvider::new(
            vec![(
                "default".into(),
                Box::new(Arc::clone(&mock)) as Box<dyn ModelProvider>,
            )],
            vec![],
            "model".into(),
        );

        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "use tools".to_string(),
        }];
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "shell",
                "description": "Run shell command",
                "parameters": {}
            }
        })];

        // chat_with_tools should delegate through the router to the mock.
        // MockModelProvider's default chat_with_tools calls chat_with_history -> chat_with_system.
        let result = router
            .chat_with_tools(&messages, &tools, "model", Some(0.7))
            .await
            .unwrap();
        assert_eq!(result.text.as_deref(), Some("tool-response"));
        assert_eq!(mock.call_count(), 1);
        assert_eq!(mock.last_model(), "model");
    }

    #[tokio::test]
    async fn chat_with_tools_routes_hint_correctly() {
        let (router, mocks) = make_router(
            vec![("fast", "fast-tool"), ("smart", "smart-tool")],
            vec![("reasoning", "smart", "claude-opus")],
        );

        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: "reason about this".to_string(),
        }];
        let tools = vec![serde_json::json!({"type": "function", "function": {"name": "test"}})];

        let result = router
            .chat_with_tools(&messages, &tools, "hint:reasoning", Some(0.5))
            .await
            .unwrap();
        assert_eq!(result.text.as_deref(), Some("smart-tool"));
        assert_eq!(mocks[1].call_count(), 1);
        assert_eq!(mocks[1].last_model(), "claude-opus");
        assert_eq!(mocks[0].call_count(), 0);
    }

    // ── Cost-optimized routing tests ────────────────────────────────

    use crate::traits::ProviderCapabilities;

    /// Mock model_provider with configurable capability flags.
    struct CapableMockModelProvider {
        response: &'static str,
        vision: bool,
        tools: bool,
    }

    impl CapableMockModelProvider {
        fn new(response: &'static str, vision: bool, tools: bool) -> Self {
            Self {
                response,
                vision,
                tools,
            }
        }
    }

    #[async_trait]
    impl ModelProvider for CapableMockModelProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: self.tools,
                vision: self.vision,
                prompt_caching: false,
            }
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: Option<f64>,
        ) -> anyhow::Result<String> {
            Ok(self.response.to_string())
        }
    }

    /// Build a per-provider pricing map for tests. Each tuple is
    /// `(provider_name, model, input_per_mtok, output_per_mtok)`.
    fn make_pricing(entries: Vec<(&str, &str, f64, f64)>) -> HashMap<String, HashMap<String, f64>> {
        let mut map: HashMap<String, HashMap<String, f64>> = HashMap::new();
        for (model_provider, model, input, output) in entries {
            let inner = map.entry(model_provider.to_string()).or_default();
            inner.insert(format!("{model}.input"), input);
            inner.insert(format!("{model}.output"), output);
        }
        map
    }

    #[test]
    fn cost_optimized_selects_cheapest_provider() {
        let model_providers: Vec<(String, Box<dyn ModelProvider>)> = vec![
            (
                "expensive".into(),
                Box::new(CapableMockModelProvider::new("exp", false, false)),
            ),
            (
                "cheap".into(),
                Box::new(CapableMockModelProvider::new("chp", false, false)),
            ),
        ];
        let routes = vec![
            (
                "expensive".to_string(),
                Route {
                    provider_name: "expensive".into(),
                    model: "big-model".into(),
                },
            ),
            (
                "cheap".to_string(),
                Route {
                    provider_name: "cheap".into(),
                    model: "small-model".into(),
                },
            ),
        ];
        let router = RouterModelProvider::new(model_providers, routes, "default-model".into());

        let prices = make_pricing(vec![
            ("expensive", "big-model", 15.0, 75.0),
            ("cheap", "small-model", 0.25, 1.25),
        ]);

        let (idx, model) =
            router.resolve_cost_optimized("hint:cost-optimized", &prices, false, false);
        assert_eq!(model, "small-model");
        assert_eq!(idx, 1);
    }

    #[test]
    fn cost_optimized_respects_vision_requirement() {
        let model_providers: Vec<(String, Box<dyn ModelProvider>)> = vec![
            (
                "no-vision".into(),
                Box::new(CapableMockModelProvider::new("nv", false, false)),
            ),
            (
                "has-vision".into(),
                Box::new(CapableMockModelProvider::new("hv", true, false)),
            ),
        ];
        let routes = vec![
            (
                "cheap".to_string(),
                Route {
                    provider_name: "no-vision".into(),
                    model: "cheap-model".into(),
                },
            ),
            (
                "vision".to_string(),
                Route {
                    provider_name: "has-vision".into(),
                    model: "vision-model".into(),
                },
            ),
        ];
        let router = RouterModelProvider::new(model_providers, routes, "default-model".into());

        let prices = make_pricing(vec![
            ("no-vision", "cheap-model", 0.10, 0.40),
            ("has-vision", "vision-model", 3.0, 15.0),
        ]);

        // With vision required, the cheap model (no vision) is filtered out
        let (_, model) = router.resolve_cost_optimized("hint:cheapest", &prices, true, false);
        assert_eq!(model, "vision-model");
    }

    #[test]
    fn cost_optimized_respects_tools_requirement() {
        let model_providers: Vec<(String, Box<dyn ModelProvider>)> = vec![
            (
                "no-tools".into(),
                Box::new(CapableMockModelProvider::new("nt", false, false)),
            ),
            (
                "has-tools".into(),
                Box::new(CapableMockModelProvider::new("ht", false, true)),
            ),
        ];
        let routes = vec![
            (
                "basic".to_string(),
                Route {
                    provider_name: "no-tools".into(),
                    model: "basic-model".into(),
                },
            ),
            (
                "tools".to_string(),
                Route {
                    provider_name: "has-tools".into(),
                    model: "tools-model".into(),
                },
            ),
        ];
        let router = RouterModelProvider::new(model_providers, routes, "default-model".into());

        let prices = make_pricing(vec![
            ("no-tools", "basic-model", 0.10, 0.40),
            ("has-tools", "tools-model", 5.0, 15.0),
        ]);

        // With tools required, the basic model (no tools) is filtered out
        let (_, model) = router.resolve_cost_optimized("hint:cost-optimized", &prices, false, true);
        assert_eq!(model, "tools-model");
    }

    #[test]
    fn cost_optimized_falls_back_when_no_pricing() {
        let (router, _) = make_router(
            vec![("default", "ok"), ("other", "ok")],
            vec![("route-a", "other", "some-model")],
        );

        // Empty pricing map — no matches possible
        let prices: HashMap<String, HashMap<String, f64>> = HashMap::new();
        let (idx, model) =
            router.resolve_cost_optimized("hint:cost-optimized", &prices, false, false);
        assert_eq!(idx, 0);
        assert_eq!(model, "default-model");
    }

    #[test]
    fn cost_optimized_with_single_route() {
        let model_providers: Vec<(String, Box<dyn ModelProvider>)> = vec![(
            "only".into(),
            Box::new(CapableMockModelProvider::new("ok", false, false)),
        )];
        let routes = vec![(
            "single".to_string(),
            Route {
                provider_name: "only".into(),
                model: "the-model".into(),
            },
        )];
        let router = RouterModelProvider::new(model_providers, routes, "default-model".into());

        let prices = make_pricing(vec![("only", "the-model", 1.0, 2.0)]);

        let (idx, model) = router.resolve_cost_optimized("hint:cheapest", &prices, false, false);
        assert_eq!(idx, 0);
        assert_eq!(model, "the-model");
    }

    #[test]
    fn cost_optimized_prefers_lower_total_cost() {
        let model_providers: Vec<(String, Box<dyn ModelProvider>)> = vec![
            (
                "p1".into(),
                Box::new(CapableMockModelProvider::new("r1", false, false)),
            ),
            (
                "p2".into(),
                Box::new(CapableMockModelProvider::new("r2", false, false)),
            ),
            (
                "p3".into(),
                Box::new(CapableMockModelProvider::new("r3", false, false)),
            ),
        ];
        let routes = vec![
            (
                "a".to_string(),
                Route {
                    provider_name: "p1".into(),
                    model: "model-a".into(),
                },
            ),
            (
                "b".to_string(),
                Route {
                    provider_name: "p2".into(),
                    model: "model-b".into(),
                },
            ),
            (
                "c".to_string(),
                Route {
                    provider_name: "p3".into(),
                    model: "model-c".into(),
                },
            ),
        ];
        let router = RouterModelProvider::new(model_providers, routes, "default-model".into());

        let prices = make_pricing(vec![
            ("p1", "model-a", 10.0, 50.0), // total: 60
            ("p2", "model-b", 0.15, 0.60), // total: 0.75 (cheapest)
            ("p3", "model-c", 3.0, 15.0),  // total: 18
        ]);

        let (idx, model) =
            router.resolve_cost_optimized("hint:cost-optimized", &prices, false, false);
        assert_eq!(model, "model-b");
        assert_eq!(idx, 1);
    }

    #[test]
    fn cost_optimized_strategy_score() {
        let prices = make_pricing(vec![
            ("cheap-provider", "cheap-model", 0.10, 0.40),
            ("expensive-provider", "expensive-model", 15.0, 75.0),
        ]);
        let strategy = CostOptimizedStrategy::new(prices);

        assert!(
            (strategy.score("cheap-provider", "cheap-model").unwrap() - 0.50).abs() < f64::EPSILON
        );
        assert!(
            (strategy
                .score("expensive-provider", "expensive-model")
                .unwrap()
                - 90.0)
                .abs()
                < f64::EPSILON
        );
        assert!(strategy.score("cheap-provider", "unknown").is_none());
        assert!(strategy.score("unknown-provider", "cheap-model").is_none());
    }

    #[tokio::test]
    async fn supports_streaming_returns_true_when_any_provider_supports_it() {
        let streaming = Arc::new(StreamingMockModelProvider::new("stream"));
        let router = RouterModelProvider::new(
            vec![
                (
                    "default".into(),
                    Box::new(MockModelProvider::new("default")) as Box<dyn ModelProvider>,
                ),
                (
                    "streaming".into(),
                    Box::new(Arc::clone(&streaming)) as Box<dyn ModelProvider>,
                ),
            ],
            vec![(
                "reasoning".into(),
                Route {
                    provider_name: "streaming".into(),
                    model: "claude-opus".into(),
                },
            )],
            "model".into(),
        );

        assert!(router.supports_streaming());
    }

    #[tokio::test]
    async fn stream_chat_with_history_routes_hint_to_correct_provider_and_model() {
        let streaming = Arc::new(StreamingMockModelProvider::new("streamed response"));
        let router = RouterModelProvider::new(
            vec![
                (
                    "default".into(),
                    Box::new(MockModelProvider::new("default")) as Box<dyn ModelProvider>,
                ),
                (
                    "streaming".into(),
                    Box::new(Arc::clone(&streaming)) as Box<dyn ModelProvider>,
                ),
            ],
            vec![(
                "reasoning".into(),
                Route {
                    provider_name: "streaming".into(),
                    model: "claude-opus".into(),
                },
            )],
            "model".into(),
        );

        let messages = vec![ChatMessage::user("hello")];
        let mut stream = router.stream_chat_with_history(
            &messages,
            "hint:reasoning",
            Some(0.0),
            StreamOptions::new(true),
        );

        let mut collected = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.expect("stream chunk should be ok");
            collected.push_str(&chunk.delta);
        }

        assert_eq!(collected, "streamed response");
        assert_eq!(streaming.stream_calls.load(Ordering::SeqCst), 1);
        assert_eq!(*streaming.last_stream_model.lock(), "claude-opus");
    }

    #[tokio::test]
    async fn stream_chat_routes_hint_with_structured_tool_events() {
        let streaming = Arc::new(ToolEventStreamingMockModelProvider::new());
        let router = RouterModelProvider::new(
            vec![
                (
                    "default".into(),
                    Box::new(MockModelProvider::new("default")) as Box<dyn ModelProvider>,
                ),
                (
                    "streaming".into(),
                    Box::new(Arc::clone(&streaming)) as Box<dyn ModelProvider>,
                ),
            ],
            vec![(
                "reasoning".into(),
                Route {
                    provider_name: "streaming".into(),
                    model: "claude-opus".into(),
                },
            )],
            "model".into(),
        );

        let messages = vec![ChatMessage::user("hello")];
        let tools = vec![ToolSpec {
            name: "shell".to_string(),
            description: "run shell commands".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" }
                }
            }),
        }];

        let mut stream = router.stream_chat(
            ChatRequest {
                messages: &messages,
                tools: Some(&tools),
            },
            "hint:reasoning",
            Some(0.0),
            StreamOptions::new(true),
        );

        let first = stream.next().await.unwrap().unwrap();
        let second = stream.next().await.unwrap().unwrap();
        assert!(stream.next().await.is_none());

        match first {
            StreamEvent::ToolCall(call) => {
                assert_eq!(call.name, "shell");
                assert_eq!(call.arguments, r#"{"command":"date"}"#);
            }
            other => panic!("expected tool-call event, got {other:?}"),
        }
        assert!(matches!(second, StreamEvent::Final));
        assert_eq!(streaming.stream_calls.load(Ordering::SeqCst), 1);
        assert_eq!(streaming.tool_event_calls.load(Ordering::SeqCst), 1);
        assert_eq!(*streaming.last_stream_model.lock(), "claude-opus");
    }
}
