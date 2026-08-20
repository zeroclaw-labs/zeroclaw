use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{FutureExt, future::join_all};
use serde_json::Value;
use std::panic::AssertUnwindSafe;

use zeroclaw_api::channel::ChannelMessage;
use zeroclaw_api::hook::ToolCallHookContext;
use zeroclaw_api::model_provider::{ChatMessage, ChatResponse};
use zeroclaw_api::tool::ToolResult;

use super::traits::{HookHandler, HookResult};

pub(crate) fn tool_call_hook_context(
    turn_id: &str,
    iteration: usize,
    call_index: usize,
) -> ToolCallHookContext {
    ToolCallHookContext::new(format!("{turn_id}:{iteration}:{call_index}"))
}

pub struct HookRunner {
    handlers: Vec<Box<dyn HookHandler>>,
}

static LEGACY_TOOL_CALL_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl Default for HookRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl HookRunner {
    /// Create an empty runner with no handlers.
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    fn next_legacy_tool_call_context() -> ToolCallHookContext {
        let sequence = LEGACY_TOOL_CALL_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        ToolCallHookContext::uncorrelated(format!("legacy:{sequence}"))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    pub fn from_config(hooks: &zeroclaw_config::schema::HooksConfig) -> Self {
        let mut runner = Self::new();
        if hooks.builtin.command_logger {
            runner.register(Box::new(super::builtin::CommandLoggerHook::new()));
        }
        if hooks.builtin.webhook_audit.enabled {
            match super::builtin::WebhookAuditHook::new(hooks.builtin.webhook_audit.clone()) {
                Ok(hook) => runner.register(Box::new(hook)),
                Err(error) => {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "hook": "webhook-audit",
                                "error": error,
                            })),
                        "webhook-audit hook configuration is invalid; hook disabled"
                    );
                }
            }
        }
        runner
    }

    /// Register a handler and re-sort by descending priority.
    pub fn register(&mut self, handler: Box<dyn HookHandler>) {
        self.handlers.push(handler);
        self.handlers
            .sort_by_key(|h| std::cmp::Reverse(h.priority()));
    }

    // ---------------------------------------------------------------
    // Void dispatchers (parallel, fire-and-forget)
    // ---------------------------------------------------------------

    pub async fn fire_gateway_start(&self, host: &str, port: u16) {
        let futs: Vec<_> = self
            .handlers
            .iter()
            .map(|h| h.on_gateway_start(host, port))
            .collect();
        join_all(futs).await;
    }

    pub async fn fire_gateway_stop(&self) {
        let futs: Vec<_> = self.handlers.iter().map(|h| h.on_gateway_stop()).collect();
        join_all(futs).await;
    }

    pub async fn fire_session_start(&self, session_id: &str, channel: &str) {
        let futs: Vec<_> = self
            .handlers
            .iter()
            .map(|h| h.on_session_start(session_id, channel))
            .collect();
        join_all(futs).await;
    }

    pub async fn fire_session_end(&self, session_id: &str, channel: &str) {
        let futs: Vec<_> = self
            .handlers
            .iter()
            .map(|h| h.on_session_end(session_id, channel))
            .collect();
        join_all(futs).await;
    }

    pub async fn fire_llm_input(&self, messages: &[ChatMessage], model: &str) {
        let futs: Vec<_> = self
            .handlers
            .iter()
            .map(|h| h.on_llm_input(messages, model))
            .collect();
        join_all(futs).await;
    }

    pub async fn fire_llm_output(&self, response: &ChatResponse) {
        let futs: Vec<_> = self
            .handlers
            .iter()
            .map(|h| h.on_llm_output(response))
            .collect();
        join_all(futs).await;
    }

    pub async fn fire_after_tool_call(&self, tool: &str, result: &ToolResult, duration: Duration) {
        let context = Self::next_legacy_tool_call_context();

        self.fire_after_tool_call_with_context(&context, tool, result, duration)
            .await;
    }

    pub async fn fire_after_tool_call_with_context(
        &self,
        context: &ToolCallHookContext,
        tool: &str,
        result: &ToolResult,
        duration: Duration,
    ) {
        let futs = self.handlers.iter().map(|h| async move {
            let hook_name = h.name();
            if AssertUnwindSafe(h.on_after_tool_call_with_context(context, tool, result, duration))
                .catch_unwind()
                .await
                .is_err()
            {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"hook": hook_name})),
                    "after_tool_call hook panicked; continuing with remaining handlers"
                );
            }
        });
        join_all(futs).await;
    }

    pub async fn fire_message_sent(&self, channel: &str, recipient: &str, content: &str) {
        let futs: Vec<_> = self
            .handlers
            .iter()
            .map(|h| h.on_message_sent(channel, recipient, content))
            .collect();
        join_all(futs).await;
    }

    pub async fn fire_heartbeat_tick(&self) {
        let futs: Vec<_> = self
            .handlers
            .iter()
            .map(|h| h.on_heartbeat_tick())
            .collect();
        join_all(futs).await;
    }

    // ---------------------------------------------------------------
    // Modifying dispatchers (sequential by priority, short-circuit on Cancel)
    // ---------------------------------------------------------------

    pub async fn run_before_model_resolve(
        &self,
        mut model_provider: String,
        mut model: String,
    ) -> HookResult<(String, String)> {
        for h in &self.handlers {
            let hook_name = h.name();
            match AssertUnwindSafe(h.before_model_resolve(model_provider.clone(), model.clone()))
                .catch_unwind()
                .await
            {
                Ok(HookResult::Continue((p, m))) => {
                    model_provider = p;
                    model = m;
                }
                Ok(HookResult::Cancel(reason)) => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"hook": hook_name, "reason": reason.to_string()})), "before_model_resolve cancelled by hook");
                    return HookResult::Cancel(reason);
                }
                Err(_) => {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"hook": hook_name})),
                        "before_model_resolve hook panicked; continuing with previous values"
                    );
                }
            }
        }
        HookResult::Continue((model_provider, model))
    }

    pub async fn run_before_prompt_build(&self, mut prompt: String) -> HookResult<String> {
        for h in &self.handlers {
            let hook_name = h.name();
            match AssertUnwindSafe(h.before_prompt_build(prompt.clone()))
                .catch_unwind()
                .await
            {
                Ok(HookResult::Continue(p)) => prompt = p,
                Ok(HookResult::Cancel(reason)) => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"hook": hook_name, "reason": reason.to_string()})), "before_prompt_build cancelled by hook");
                    return HookResult::Cancel(reason);
                }
                Err(_) => {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"hook": hook_name})),
                        "before_prompt_build hook panicked; continuing with previous value"
                    );
                }
            }
        }
        HookResult::Continue(prompt)
    }

    pub async fn run_before_llm_call(
        &self,
        messages: &mut Vec<ChatMessage>,
        model: &mut String,
    ) -> HookResult<()> {
        for h in &self.handlers {
            let hook_name = h.name();
            let mut candidate_messages = messages.clone();
            let mut candidate_model = model.clone();
            match AssertUnwindSafe(h.before_llm_call(&mut candidate_messages, &mut candidate_model))
                .catch_unwind()
                .await
            {
                Ok(HookResult::Continue(())) => {
                    *messages = candidate_messages;
                    *model = candidate_model;
                }
                Ok(HookResult::Cancel(reason)) => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"hook": hook_name, "reason": reason.to_string()})), "before_llm_call cancelled by hook");
                    return HookResult::Cancel(reason);
                }
                Err(_) => {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"hook": hook_name})),
                        "before_llm_call hook panicked; continuing with previous values"
                    );
                }
            }
        }
        HookResult::Continue(())
    }

    pub async fn run_before_tool_call(
        &self,
        name: String,
        args: Value,
    ) -> HookResult<(String, Value)> {
        let context = Self::next_legacy_tool_call_context();
        self.run_before_tool_call_with_context(&context, name, args)
            .await
    }

    pub async fn run_before_tool_call_with_context(
        &self,
        context: &ToolCallHookContext,
        mut name: String,
        mut args: Value,
    ) -> HookResult<(String, Value)> {
        for h in &self.handlers {
            let hook_name = h.name();
            match AssertUnwindSafe(h.before_tool_call_with_context(
                context,
                name.clone(),
                args.clone(),
            ))
            .catch_unwind()
            .await
            {
                Ok(HookResult::Continue((n, a))) => {
                    name = n;
                    args = a;
                }
                Ok(HookResult::Cancel(reason)) => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"hook": hook_name, "reason": reason.to_string()})), "before_tool_call cancelled by hook");
                    return HookResult::Cancel(reason);
                }
                Err(_) => {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"hook": hook_name})),
                        "before_tool_call hook panicked; continuing with previous values"
                    );
                }
            }
        }
        HookResult::Continue((name, args))
    }

    pub async fn run_on_message_received(
        &self,
        mut message: ChannelMessage,
    ) -> HookResult<ChannelMessage> {
        for h in &self.handlers {
            let hook_name = h.name();
            match AssertUnwindSafe(h.on_message_received(message.clone()))
                .catch_unwind()
                .await
            {
                Ok(HookResult::Continue(m)) => message = m,
                Ok(HookResult::Cancel(reason)) => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"hook": hook_name, "reason": reason.to_string()})), "on_message_received cancelled by hook");
                    return HookResult::Cancel(reason);
                }
                Err(_) => {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"hook": hook_name})),
                        "on_message_received hook panicked; continuing with previous message"
                    );
                }
            }
        }
        HookResult::Continue(message)
    }

    pub async fn run_on_message_sending(
        &self,
        mut channel: String,
        mut recipient: String,
        mut content: String,
    ) -> HookResult<(String, String, String)> {
        for h in &self.handlers {
            let hook_name = h.name();
            match AssertUnwindSafe(h.on_message_sending(
                channel.clone(),
                recipient.clone(),
                content.clone(),
            ))
            .catch_unwind()
            .await
            {
                Ok(HookResult::Continue((c, r, ct))) => {
                    channel = c;
                    recipient = r;
                    content = ct;
                }
                Ok(HookResult::Cancel(reason)) => {
                    ::zeroclaw_log::record!(INFO, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"hook": hook_name, "reason": reason.to_string()})), "on_message_sending cancelled by hook");
                    return HookResult::Cancel(reason);
                }
                Err(_) => {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({"hook": hook_name})),
                        "on_message_sending hook panicked; continuing with previous message"
                    );
                }
            }
        }
        HookResult::Continue((channel, recipient, content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    /// A hook that records how many times void events fire.
    struct CountingHook {
        name: String,
        priority: i32,
        fire_count: Arc<AtomicU32>,
    }

    impl CountingHook {
        fn new(name: &str, priority: i32) -> (Self, Arc<AtomicU32>) {
            let count = Arc::new(AtomicU32::new(0));
            (
                Self {
                    name: name.to_string(),
                    priority,
                    fire_count: count.clone(),
                },
                count,
            )
        }
    }

    #[async_trait]
    impl HookHandler for CountingHook {
        fn name(&self) -> &str {
            &self.name
        }
        fn priority(&self) -> i32 {
            self.priority
        }
        async fn on_heartbeat_tick(&self) {
            self.fire_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// A modifying hook that uppercases the prompt.
    struct UppercasePromptHook {
        name: String,
        priority: i32,
    }

    #[async_trait]
    impl HookHandler for UppercasePromptHook {
        fn name(&self) -> &str {
            &self.name
        }
        fn priority(&self) -> i32 {
            self.priority
        }
        async fn before_prompt_build(&self, prompt: String) -> HookResult<String> {
            HookResult::Continue(prompt.to_uppercase())
        }
    }

    /// A modifying hook that cancels before_prompt_build.
    struct CancelPromptHook {
        name: String,
        priority: i32,
    }

    #[async_trait]
    impl HookHandler for CancelPromptHook {
        fn name(&self) -> &str {
            &self.name
        }
        fn priority(&self) -> i32 {
            self.priority
        }
        async fn before_prompt_build(&self, _prompt: String) -> HookResult<String> {
            HookResult::Cancel("blocked by policy".into())
        }
    }

    /// A modifying hook that appends a suffix to the prompt.
    struct SuffixPromptHook {
        name: String,
        priority: i32,
        suffix: String,
    }

    #[async_trait]
    impl HookHandler for SuffixPromptHook {
        fn name(&self) -> &str {
            &self.name
        }
        fn priority(&self) -> i32 {
            self.priority
        }
        async fn before_prompt_build(&self, prompt: String) -> HookResult<String> {
            HookResult::Continue(format!("{}{}", prompt, self.suffix))
        }
    }

    #[test]
    fn register_and_sort_by_priority() {
        let mut runner = HookRunner::new();
        let (low, _) = CountingHook::new("low", 1);
        let (high, _) = CountingHook::new("high", 10);
        let (mid, _) = CountingHook::new("mid", 5);

        runner.register(Box::new(low));
        runner.register(Box::new(high));
        runner.register(Box::new(mid));

        let names: Vec<&str> = runner.handlers.iter().map(|h| h.name()).collect();
        assert_eq!(names, vec!["high", "mid", "low"]);
    }

    #[tokio::test]
    async fn void_hooks_fire_all_handlers() {
        let mut runner = HookRunner::new();
        let (h1, c1) = CountingHook::new("hook_a", 0);
        let (h2, c2) = CountingHook::new("hook_b", 0);

        runner.register(Box::new(h1));
        runner.register(Box::new(h2));

        runner.fire_heartbeat_tick().await;

        assert_eq!(c1.load(Ordering::SeqCst), 1);
        assert_eq!(c2.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn modifying_hook_can_cancel() {
        let mut runner = HookRunner::new();
        runner.register(Box::new(CancelPromptHook {
            name: "blocker".into(),
            priority: 10,
        }));
        runner.register(Box::new(UppercasePromptHook {
            name: "upper".into(),
            priority: 0,
        }));

        let result = runner.run_before_prompt_build("hello".into()).await;
        assert!(result.is_cancel());
    }

    #[tokio::test]
    async fn modifying_hook_pipelines_data() {
        let mut runner = HookRunner::new();

        // Priority 10 runs first: uppercases
        runner.register(Box::new(UppercasePromptHook {
            name: "upper".into(),
            priority: 10,
        }));
        // Priority 0 runs second: appends suffix
        runner.register(Box::new(SuffixPromptHook {
            name: "suffix".into(),
            priority: 0,
            suffix: "_done".into(),
        }));

        match runner.run_before_prompt_build("hello".into()).await {
            HookResult::Continue(result) => assert_eq!(result, "HELLO_done"),
            HookResult::Cancel(_) => panic!("should not cancel"),
        }
    }

    /// A hook that panics on a configurable method. Records nothing; its
    /// only role is to exercise the `catch_unwind` branch in the runner.
    struct PanickingHook {
        name: String,
        priority: i32,
    }

    #[async_trait]
    impl HookHandler for PanickingHook {
        fn name(&self) -> &str {
            &self.name
        }
        fn priority(&self) -> i32 {
            self.priority
        }

        async fn before_model_resolve(
            &self,
            _model_provider: String,
            _model: String,
        ) -> HookResult<(String, String)> {
            panic!("simulated before_model_resolve panic");
        }

        async fn before_tool_call(
            &self,
            _name: String,
            _args: Value,
        ) -> HookResult<(String, Value)> {
            panic!("simulated before_tool_call panic");
        }

        async fn before_llm_call(
            &self,
            _messages: &mut Vec<ChatMessage>,
            _model: &mut String,
        ) -> HookResult<()> {
            panic!("simulated before_llm_call panic");
        }

        async fn on_message_received(
            &self,
            _message: ChannelMessage,
        ) -> HookResult<ChannelMessage> {
            panic!("simulated on_message_received panic");
        }
    }

    /// A hook that cancels the run on a configurable method.
    struct CancelNonPromptHook {
        name: String,
        priority: i32,
    }

    #[async_trait]
    impl HookHandler for CancelNonPromptHook {
        fn name(&self) -> &str {
            &self.name
        }
        fn priority(&self) -> i32 {
            self.priority
        }

        async fn before_llm_call(
            &self,
            _messages: &mut Vec<ChatMessage>,
            _model: &mut String,
        ) -> HookResult<()> {
            HookResult::Cancel("blocked by non-prompt cancel hook".into())
        }

        async fn on_message_received(
            &self,
            _message: ChannelMessage,
        ) -> HookResult<ChannelMessage> {
            HookResult::Cancel("blocked by non-prompt cancel hook".into())
        }
    }

    #[tokio::test]
    async fn panicking_before_model_resolve_does_not_break_subsequent_handler() {
        let mut runner = HookRunner::new();
        // Higher priority panics first; lower priority must still run.
        runner.register(Box::new(PanickingHook {
            name: "panicker".into(),
            priority: 10,
        }));
        runner.register(Box::new(UppercasePromptHook {
            name: "upper".into(),
            priority: 0,
        }));

        struct ModelConstHook {
            name: String,
            priority: i32,
        }
        #[async_trait]
        impl HookHandler for ModelConstHook {
            fn name(&self) -> &str {
                &self.name
            }
            fn priority(&self) -> i32 {
                self.priority
            }
            async fn before_model_resolve(
                &self,
                _provider: String,
                _model: String,
            ) -> HookResult<(String, String)> {
                HookResult::Continue(("const_provider".into(), "const_model".into()))
            }
        }

        runner.register(Box::new(ModelConstHook {
            name: "const".into(),
            priority: 0,
        }));

        let result = runner
            .run_before_model_resolve("openai".into(), "gpt-4o".into())
            .await;
        // The panicker panics (catch_unwind recovers), the const hook runs
        // and overrides the values. Final tuple is the const values.
        match result {
            HookResult::Continue((p, m)) => {
                assert_eq!(p, "const_provider");
                assert_eq!(m, "const_model");
            }
            HookResult::Cancel(_) => panic!("panicking hook must not cancel"),
        }
    }

    #[tokio::test]
    async fn panicking_before_tool_call_does_not_break_subsequent_handler() {
        let mut runner = HookRunner::new();
        runner.register(Box::new(PanickingHook {
            name: "panicker".into(),
            priority: 10,
        }));

        // A modifying hook that renames the tool call so we can verify it
        // ran after the panicker.
        struct RenameToolHook {
            name: String,
            priority: i32,
        }
        #[async_trait]
        impl HookHandler for RenameToolHook {
            fn name(&self) -> &str {
                &self.name
            }
            fn priority(&self) -> i32 {
                self.priority
            }
            async fn before_tool_call(
                &self,
                name: String,
                _args: Value,
            ) -> HookResult<(String, Value)> {
                HookResult::Continue((format!("{name}_renamed"), Value::Null))
            }
        }

        runner.register(Box::new(RenameToolHook {
            name: "renamer".into(),
            priority: 0,
        }));

        let result = runner
            .run_before_tool_call("shell".into(), Value::Null)
            .await;
        match result {
            HookResult::Continue((name, _)) => {
                assert_eq!(
                    name, "shell_renamed",
                    "hook after panicker must run and apply its modification"
                );
            }
            HookResult::Cancel(_) => panic!("panicking hook must not cancel"),
        }
    }

    #[test]
    fn tool_call_hook_context_distinguishes_turn_positions() {
        let first = tool_call_hook_context("turn-a", 0, 0);
        let next_call = tool_call_hook_context("turn-a", 0, 1);
        let next_iteration = tool_call_hook_context("turn-a", 1, 0);
        let next_turn = tool_call_hook_context("turn-b", 0, 0);

        assert_ne!(first, next_call);
        assert_ne!(first, next_iteration);
        assert_ne!(first, next_turn);
    }

    #[tokio::test]
    async fn context_aware_runner_dispatches_legacy_tool_hooks() {
        struct LegacyToolHook {
            calls: Arc<Mutex<Vec<String>>>,
        }

        #[async_trait]
        impl HookHandler for LegacyToolHook {
            fn name(&self) -> &str {
                "legacy-tool"
            }

            async fn before_tool_call(
                &self,
                name: String,
                args: Value,
            ) -> HookResult<(String, Value)> {
                self.calls.lock().unwrap().push(format!("before:{name}"));
                HookResult::Continue((name, args))
            }

            async fn on_after_tool_call(
                &self,
                tool: &str,
                _result: &ToolResult,
                _duration: Duration,
            ) {
                self.calls.lock().unwrap().push(format!("after:{tool}"));
            }
        }

        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut runner = HookRunner::new();
        runner.register(Box::new(LegacyToolHook {
            calls: Arc::clone(&calls),
        }));
        let context = tool_call_hook_context("turn-a", 0, 0);
        let result = runner
            .run_before_tool_call_with_context(&context, "shell".into(), Value::Null)
            .await;
        assert!(!result.is_cancel());

        let tool_result = ToolResult {
            success: true,
            output: "ok".into(),
            error: None,
        };
        runner
            .fire_after_tool_call_with_context(
                &context,
                "shell",
                &tool_result,
                Duration::from_millis(3),
            )
            .await;

        assert_eq!(
            *calls.lock().unwrap(),
            vec!["before:shell".to_string(), "after:shell".to_string()]
        );
    }

    #[tokio::test]
    async fn legacy_runner_dispatches_context_only_hooks_without_false_correlation() {
        struct ContextOnlyHook {
            calls: Arc<Mutex<Vec<(String, String, bool)>>>,
        }

        #[async_trait]
        impl HookHandler for ContextOnlyHook {
            fn name(&self) -> &str {
                "context-only"
            }

            async fn before_tool_call_with_context(
                &self,
                context: &ToolCallHookContext,
                name: String,
                args: Value,
            ) -> HookResult<(String, Value)> {
                self.calls.lock().unwrap().push((
                    "before".to_string(),
                    context.invocation_id().to_string(),
                    context.is_correlated(),
                ));
                HookResult::Continue((name, args))
            }

            async fn on_after_tool_call_with_context(
                &self,
                context: &ToolCallHookContext,
                _tool: &str,
                _result: &ToolResult,
                _duration: Duration,
            ) {
                self.calls.lock().unwrap().push((
                    "after".to_string(),
                    context.invocation_id().to_string(),
                    context.is_correlated(),
                ));
            }
        }

        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut runner = HookRunner::new();
        runner.register(Box::new(ContextOnlyHook {
            calls: Arc::clone(&calls),
        }));

        let before = runner
            .run_before_tool_call("shell".into(), Value::Null)
            .await;
        assert!(!before.is_cancel());
        runner
            .fire_after_tool_call(
                "shell",
                &ToolResult {
                    success: true,
                    output: "ok".into(),
                    error: None,
                },
                Duration::ZERO,
            )
            .await;

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "before");
        assert_eq!(calls[1].0, "after");
        assert!(!calls[0].2);
        assert!(!calls[1].2);
        assert_ne!(calls[0].1, calls[1].1);
    }

    #[tokio::test]
    async fn legacy_context_ids_are_unique_across_runners() {
        struct CaptureContext(Arc<Mutex<Vec<String>>>);

        #[async_trait]
        impl HookHandler for CaptureContext {
            fn name(&self) -> &str {
                "capture-context"
            }

            async fn before_tool_call_with_context(
                &self,
                context: &ToolCallHookContext,
                name: String,
                args: Value,
            ) -> HookResult<(String, Value)> {
                self.0
                    .lock()
                    .unwrap()
                    .push(context.invocation_id().to_string());
                HookResult::Continue((name, args))
            }
        }

        let contexts = Arc::new(Mutex::new(Vec::new()));
        let mut first = HookRunner::new();
        first.register(Box::new(CaptureContext(Arc::clone(&contexts))));
        let mut second = HookRunner::new();
        second.register(Box::new(CaptureContext(Arc::clone(&contexts))));

        assert!(
            !first
                .run_before_tool_call("shell".into(), Value::Null)
                .await
                .is_cancel()
        );
        assert!(
            !second
                .run_before_tool_call("shell".into(), Value::Null)
                .await
                .is_cancel()
        );

        let contexts = contexts.lock().unwrap();
        assert_eq!(contexts.len(), 2);
        assert_ne!(contexts[0], contexts[1]);
    }

    #[tokio::test]
    async fn context_aware_after_dispatch_continues_after_handler_panic() {
        struct PanickingAfterHook;
        #[async_trait]
        impl HookHandler for PanickingAfterHook {
            fn name(&self) -> &str {
                "panicking-after"
            }

            fn priority(&self) -> i32 {
                10
            }

            async fn on_after_tool_call(
                &self,
                _tool: &str,
                _result: &ToolResult,
                _duration: Duration,
            ) {
                panic!("simulated after_tool_call panic");
            }
        }

        struct CountingAfterHook(Arc<AtomicU32>);
        #[async_trait]
        impl HookHandler for CountingAfterHook {
            fn name(&self) -> &str {
                "counting-after"
            }

            async fn on_after_tool_call(
                &self,
                _tool: &str,
                _result: &ToolResult,
                _duration: Duration,
            ) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let count = Arc::new(AtomicU32::new(0));
        let mut runner = HookRunner::new();
        runner.register(Box::new(PanickingAfterHook));
        runner.register(Box::new(CountingAfterHook(Arc::clone(&count))));
        let result = ToolResult {
            success: true,
            output: "ok".into(),
            error: None,
        };

        runner
            .fire_after_tool_call_with_context(
                &tool_call_hook_context("turn-a", 0, 0),
                "shell",
                &result,
                Duration::ZERO,
            )
            .await;

        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancelling_before_llm_call_short_circuits_remaining_handlers() {
        let mut runner = HookRunner::new();
        // CancelNonPromptHook overrides before_llm_call to return Cancel.
        runner.register(Box::new(CancelNonPromptHook {
            name: "blocker".into(),
            priority: 10,
        }));

        // A second hook that overrides before_llm_call; we count its calls
        // to verify it did NOT run after the canceller.
        struct LlmCallCounterHook {
            name: String,
            priority: i32,
            count: Arc<AtomicU32>,
        }
        #[async_trait]
        impl HookHandler for LlmCallCounterHook {
            fn name(&self) -> &str {
                &self.name
            }
            fn priority(&self) -> i32 {
                self.priority
            }
            async fn before_llm_call(
                &self,
                _messages: &mut Vec<ChatMessage>,
                _model: &mut String,
            ) -> HookResult<()> {
                self.count.fetch_add(1, Ordering::SeqCst);
                HookResult::Continue(())
            }
        }

        let count = Arc::new(AtomicU32::new(0));
        runner.register(Box::new(LlmCallCounterHook {
            name: "counter".into(),
            priority: 0,
            count: Arc::clone(&count),
        }));

        let mut messages = vec![ChatMessage {
            role: "user".into(),
            content: "hi".into(),
        }];
        let mut model = "gpt-4o".into();
        let result = runner.run_before_llm_call(&mut messages, &mut model).await;

        assert!(
            result.is_cancel(),
            "canceller must short-circuit the run with HookResult::Cancel"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "hooks after the canceller must NOT run"
        );
    }

    #[tokio::test]
    async fn panicking_before_llm_call_discards_partial_mutations() {
        struct MutateThenPanicHook;

        #[async_trait]
        impl HookHandler for MutateThenPanicHook {
            fn name(&self) -> &str {
                "mutate-then-panic"
            }

            async fn before_llm_call(
                &self,
                messages: &mut Vec<ChatMessage>,
                model: &mut String,
            ) -> HookResult<()> {
                messages[0].content = "partial mutation".into();
                *model = "partial-model".into();
                panic!("hook panic after mutation");
            }
        }

        let mut runner = HookRunner::new();
        runner.register(Box::new(MutateThenPanicHook));
        let mut messages = vec![ChatMessage {
            role: "user".into(),
            content: "original request".into(),
        }];
        let mut model = "original-model".into();

        let result = runner.run_before_llm_call(&mut messages, &mut model).await;

        assert!(matches!(result, HookResult::Continue(())));
        assert_eq!(messages[0].content, "original request");
        assert_eq!(model, "original-model");
    }

    #[tokio::test]
    async fn cancelling_on_message_received_short_circuits_remaining_handlers() {
        // Same contract verified on a non-modifying-family hook to pin
        // consistent cancellation behavior across hook families.
        struct CancelMessageHook {
            name: String,
            priority: i32,
        }
        #[async_trait]
        impl HookHandler for CancelMessageHook {
            fn name(&self) -> &str {
                &self.name
            }
            fn priority(&self) -> i32 {
                self.priority
            }
            async fn on_message_received(
                &self,
                _message: ChannelMessage,
            ) -> HookResult<ChannelMessage> {
                HookResult::Cancel("blocked by on_message_received cancel".into())
            }
        }

        let mut runner = HookRunner::new();
        runner.register(Box::new(CancelMessageHook {
            name: "blocker".into(),
            priority: 10,
        }));

        // A no-op subsequent handler counted to confirm short-circuit.
        struct PassThroughMessageHook {
            name: String,
            priority: i32,
            count: Arc<AtomicU32>,
        }
        #[async_trait]
        impl HookHandler for PassThroughMessageHook {
            fn name(&self) -> &str {
                &self.name
            }
            fn priority(&self) -> i32 {
                self.priority
            }
            async fn on_message_received(
                &self,
                message: ChannelMessage,
            ) -> HookResult<ChannelMessage> {
                self.count.fetch_add(1, Ordering::SeqCst);
                HookResult::Continue(message)
            }
        }

        let count = Arc::new(AtomicU32::new(0));
        runner.register(Box::new(PassThroughMessageHook {
            name: "passthrough".into(),
            priority: 0,
            count: Arc::clone(&count),
        }));

        let result = runner
            .run_on_message_received(ChannelMessage::default())
            .await;

        assert!(
            result.is_cancel(),
            "on_message_received canceller must short-circuit"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "pass-through hook after the canceller must NOT run"
        );
    }

    // ── from_config and lifecycle tests ──────────────────────────

    struct SessionCountingHook {
        name: String,
        start_count: Arc<AtomicU32>,
        end_count: Arc<AtomicU32>,
    }

    impl SessionCountingHook {
        fn new(name: &str) -> (Self, Arc<AtomicU32>, Arc<AtomicU32>) {
            let start = Arc::new(AtomicU32::new(0));
            let end = Arc::new(AtomicU32::new(0));
            (
                Self {
                    name: name.to_string(),
                    start_count: start.clone(),
                    end_count: end.clone(),
                },
                start,
                end,
            )
        }
    }

    #[async_trait]
    impl HookHandler for SessionCountingHook {
        fn name(&self) -> &str {
            &self.name
        }
        fn priority(&self) -> i32 {
            0
        }
        async fn on_session_start(&self, _session_id: &str, _channel: &str) {
            self.start_count.fetch_add(1, Ordering::SeqCst);
        }
        async fn on_session_end(&self, _session_id: &str, _channel: &str) {
            self.end_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn from_config_disabled_builtins_produces_empty_runner() {
        let config = zeroclaw_config::schema::HooksConfig {
            enabled: true,
            builtin: zeroclaw_config::schema::BuiltinHooksConfig {
                command_logger: false,
                webhook_audit: zeroclaw_config::schema::WebhookAuditConfig::default(),
            },
        };
        let runner = HookRunner::from_config(&config);
        assert!(
            runner.handlers.is_empty(),
            "no builtins enabled → runner must be empty"
        );
    }

    #[test]
    fn from_config_registers_command_logger_when_enabled() {
        let config = zeroclaw_config::schema::HooksConfig {
            enabled: true,
            builtin: zeroclaw_config::schema::BuiltinHooksConfig {
                command_logger: true,
                webhook_audit: zeroclaw_config::schema::WebhookAuditConfig::default(),
            },
        };
        let runner = HookRunner::from_config(&config);
        let names: Vec<&str> = runner.handlers.iter().map(|h| h.name()).collect();
        assert!(
            names.contains(&"command-logger"),
            "command-logger enabled → must be registered; got {names:?}"
        );
    }

    #[test]
    fn from_config_skips_invalid_webhook_and_keeps_valid_builtins() {
        let config = zeroclaw_config::schema::HooksConfig {
            enabled: true,
            builtin: zeroclaw_config::schema::BuiltinHooksConfig {
                command_logger: true,
                webhook_audit: zeroclaw_config::schema::WebhookAuditConfig {
                    enabled: true,
                    url: "http://example.com/audit".to_string(),
                    ..Default::default()
                },
            },
        };

        let runner = HookRunner::from_config(&config);
        let names: Vec<&str> = runner.handlers.iter().map(|h| h.name()).collect();

        assert_eq!(names, vec!["command-logger"]);
    }

    #[tokio::test]
    async fn session_lifecycle_events_reach_registered_handler() {
        let mut runner = HookRunner::new();
        let (hook, start_count, end_count) = SessionCountingHook::new("session-watcher");
        runner.register(Box::new(hook));

        runner.fire_session_start("sess-1", "rpc").await;
        assert_eq!(start_count.load(Ordering::SeqCst), 1);

        runner.fire_session_end("sess-1", "rpc").await;
        assert_eq!(end_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn empty_runner_lifecycle_events_are_noops() {
        let runner = HookRunner::new();
        // Must not panic when no handlers are registered.
        runner.fire_session_start("sess-1", "rpc").await;
        runner.fire_session_end("sess-1", "rpc").await;
    }
}
