use std::time::Duration;

use futures_util::{FutureExt, future::join_all};
use serde_json::Value;
use std::panic::AssertUnwindSafe;

use zeroclaw_api::channel::ChannelMessage;
use zeroclaw_api::model_provider::{ChatMessage, ChatResponse};
use zeroclaw_api::tool::ToolResult;

use super::traits::{HookHandler, HookResult};

/// Dispatcher that manages registered hook handlers.
///
/// Void hooks are dispatched in parallel via `join_all`.
/// Modifying hooks run sequentially by priority (higher first), piping output
/// and short-circuiting on `Cancel`.
pub struct HookRunner {
    handlers: Vec<Box<dyn HookHandler>>,
}

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
        let futs: Vec<_> = self
            .handlers
            .iter()
            .map(|h| h.on_after_tool_call(tool, result, duration))
            .collect();
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
            match AssertUnwindSafe(h.before_llm_call(messages, model))
                .catch_unwind()
                .await
            {
                Ok(HookResult::Continue(())) => {}
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
        mut name: String,
        mut args: Value,
    ) -> HookResult<(String, Value)> {
        for h in &self.handlers {
            let hook_name = h.name();
            match AssertUnwindSafe(h.before_tool_call(name.clone(), args.clone()))
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

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

    // ── Panic recovery + cancellation propagation (#7688) ────────────────────
    //
    // Pinned regression: a hook that panics must not abort the runner or
    // prevent subsequent handlers in the same `run_*` call from running, and
    // a hook that returns `HookResult::Cancel(_)` must short-circuit the
    // remaining handlers in the same call. These contracts are spelled out
    // at lines 144–156 (cancel) and 148–156 (panic recovery) for
    // `run_before_model_resolve`, and duplicated in every other `run_*`
    // method on `HookRunner`. Without focused tests, a future refactor that
    // drops the catch_unwind arm as "seems redundant because hook code
    // shouldn't panic" would silently regress runtime control flow.
    //
    // We deliberately cover a small representative set of hook families
    // rather than all six, matching the issue acceptance criteria ("tests
    // document any intentional asymmetry between hook families").

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

        // `before_model_resolve` returns the (provider, model) tuple; the
        // panicker yields no value so the runner falls back to the prior
        // (input) values and the subsequent UppercasePromptHook ... wait,
        // UppercasePromptHook only overrides before_prompt_build. Use a
        // hook that does override before_model_resolve so the "subsequent
        // handler ran" assertion is meaningful.
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
}
