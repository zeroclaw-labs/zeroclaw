use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use zeroclaw::hooks::{HookHandler, HookResult, HookRunner};
use zeroclaw::providers::traits::{ChatMessage, ChatResponse, TokenUsage};

/// Test hook that counts llm_output fires
struct LlmOutputCounter {
    count: Arc<AtomicUsize>,
    _last_model: Arc<parking_lot::Mutex<Option<String>>>,
}

#[async_trait]
impl HookHandler for LlmOutputCounter {
    fn name(&self) -> &str {
        "llm-output-counter"
    }

    async fn on_llm_output(&self, _response: &ChatResponse) {
        self.count.fetch_add(1, Ordering::SeqCst);
        // Extract model from response metadata if available
        // For this test, we just verify the hook fires
    }
}

/// Test hook that modifies messages before LLM call
struct MessageModifier {
    suffix: String,
}

#[async_trait]
impl HookHandler for MessageModifier {
    fn name(&self) -> &str {
        "message-modifier"
    }

    fn priority(&self) -> i32 {
        10
    }

    async fn before_llm_call(
        &self,
        mut messages: Vec<ChatMessage>,
        model: String,
    ) -> HookResult<(Vec<ChatMessage>, String)> {
        // Modify the last user message
        if let Some(last) = messages.last_mut() {
            last.content = format!("{} {}", last.content, self.suffix);
        }
        HookResult::Continue((messages, model))
    }
}

/// Test hook that cancels LLM call
struct LlmCallCanceller {
    should_cancel: Arc<AtomicBool>,
}

#[async_trait]
impl HookHandler for LlmCallCanceller {
    fn name(&self) -> &str {
        "llm-call-canceller"
    }

    fn priority(&self) -> i32 {
        100
    }

    async fn before_llm_call(
        &self,
        messages: Vec<ChatMessage>,
        model: String,
    ) -> HookResult<(Vec<ChatMessage>, String)> {
        if self.should_cancel.load(Ordering::SeqCst) {
            HookResult::Cancel("LLM call blocked by policy".to_string())
        } else {
            HookResult::Continue((messages, model))
        }
    }
}

/// Test hook that overrides model selection
struct ModelOverrider {
    target_model: String,
}

#[async_trait]
impl HookHandler for ModelOverrider {
    fn name(&self) -> &str {
        "model-overrider"
    }

    fn priority(&self) -> i32 {
        50
    }

    async fn before_model_resolve(
        &self,
        provider: String,
        _model: String,
    ) -> HookResult<(String, String)> {
        HookResult::Continue((provider, self.target_model.clone()))
    }
}

/// Test hook that injects context into prompt
struct PromptInjector {
    injection: String,
}

#[async_trait]
impl HookHandler for PromptInjector {
    fn name(&self) -> &str {
        "prompt-injector"
    }

    fn priority(&self) -> i32 {
        20
    }

    async fn before_prompt_build(&self, prompt: String) -> HookResult<String> {
        HookResult::Continue(format!("{}\n\n{}", prompt, self.injection))
    }
}

/// Test hook that panics to verify panic isolation
struct PanicHook;

#[async_trait]
impl HookHandler for PanicHook {
    fn name(&self) -> &str {
        "panic-hook"
    }

    async fn before_prompt_build(&self, _prompt: String) -> HookResult<String> {
        panic!("intentional panic for testing");
    }
}

#[tokio::test]
async fn fire_llm_output_fires() {
    let count = Arc::new(AtomicUsize::new(0));
    let last_model = Arc::new(parking_lot::Mutex::new(None));

    let mut runner = HookRunner::new();
    runner.register(Box::new(LlmOutputCounter {
        count: count.clone(),
        _last_model: last_model.clone(),
    }));

    // Simulate LLM response
    let response = ChatResponse {
        text: Some("Hello, world!".to_string()),
        tool_calls: vec![],
        usage: Some(TokenUsage {
            input_tokens: Some(10),
            output_tokens: Some(5),
            cached_input_tokens: None,
        }),
        reasoning_content: None,
    };

    runner.fire_llm_output(&response).await;

    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn run_before_llm_call_can_modify_messages() {
    let mut runner = HookRunner::new();
    runner.register(Box::new(MessageModifier {
        suffix: "[MODIFIED]".to_string(),
    }));

    let messages = vec![ChatMessage::user("Original message")];
    let model = "gpt-4".to_string();

    match runner.run_before_llm_call(messages, model).await {
        HookResult::Continue((modified_messages, _)) => {
            assert_eq!(modified_messages.len(), 1);
            assert_eq!(modified_messages[0].content, "Original message [MODIFIED]");
        }
        HookResult::Cancel(_) => panic!("should not cancel"),
    }
}

#[tokio::test]
async fn run_before_llm_call_can_cancel() {
    let should_cancel = Arc::new(AtomicBool::new(true));

    let mut runner = HookRunner::new();
    runner.register(Box::new(LlmCallCanceller {
        should_cancel: should_cancel.clone(),
    }));

    let messages = vec![ChatMessage::user("test")];
    let model = "gpt-4".to_string();

    match runner.run_before_llm_call(messages, model).await {
        HookResult::Continue(_) => panic!("should cancel"),
        HookResult::Cancel(reason) => {
            assert_eq!(reason, "LLM call blocked by policy");
        }
    }
}

#[tokio::test]
async fn run_before_model_resolve_can_override_model() {
    let mut runner = HookRunner::new();
    runner.register(Box::new(ModelOverrider {
        target_model: "gpt-3.5-turbo".to_string(),
    }));

    let provider = "openai".to_string();
    let model = "gpt-4".to_string();

    match runner.run_before_model_resolve(provider, model).await {
        HookResult::Continue((_, new_model)) => {
            assert_eq!(new_model, "gpt-3.5-turbo");
        }
        HookResult::Cancel(_) => panic!("should not cancel"),
    }
}

#[tokio::test]
async fn run_before_prompt_build_can_inject_context() {
    let mut runner = HookRunner::new();
    runner.register(Box::new(PromptInjector {
        injection: "INJECTED CONTEXT".to_string(),
    }));

    let prompt = "Base prompt".to_string();

    match runner.run_before_prompt_build(prompt).await {
        HookResult::Continue(modified) => {
            assert!(modified.contains("Base prompt"));
            assert!(modified.contains("INJECTED CONTEXT"));
        }
        HookResult::Cancel(_) => panic!("should not cancel"),
    }
}

#[tokio::test]
async fn hook_panic_does_not_crash_runner() {
    let mut runner = HookRunner::new();
    runner.register(Box::new(PanicHook));
    runner.register(Box::new(PromptInjector {
        injection: "AFTER_PANIC".to_string(),
    }));

    let prompt = "Base".to_string();

    // Even though PanicHook panics, the runner should continue with remaining hooks
    match runner.run_before_prompt_build(prompt).await {
        HookResult::Continue(modified) => {
            // The second hook should still run
            assert!(modified.contains("AFTER_PANIC"));
        }
        HookResult::Cancel(_) => panic!("should not cancel"),
    }
}
