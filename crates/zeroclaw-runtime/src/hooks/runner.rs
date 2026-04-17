use std::time::Duration;

use futures_util::{FutureExt, future::join_all};
use serde_json::Value;
use std::panic::AssertUnwindSafe;
use tracing::info;

use zeroclaw_api::channel::ChannelMessage;
use zeroclaw_api::provider::{ChatMessage, ChatResponse};
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
        mut provider: String,
        mut model: String,
    ) -> HookResult<(String, String)> {
        for h in &self.handlers {
            let hook_name = h.name();
            match AssertUnwindSafe(h.before_model_resolve(provider.clone(), model.clone()))
                .catch_unwind()
                .await
            {
                Ok(HookResult::Continue((p, m))) => {
                    provider = p;
                    model = m;
                }
                Ok(HookResult::Cancel(reason)) => {
                    info!(
                        hook = hook_name,
                        reason, "before_model_resolve cancelled by hook"
                    );
                    return HookResult::Cancel(reason);
                }
                Err(_) => {
                    tracing::error!(
                        hook = hook_name,
                        "before_model_resolve hook panicked; continuing with previous values"
                    );
                }
            }
        }
        HookResult::Continue((provider, model))
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
                    info!(
                        hook = hook_name,
                        reason, "before_prompt_build cancelled by hook"
                    );
                    return HookResult::Cancel(reason);
                }
                Err(_) => {
                    tracing::error!(
                        hook = hook_name,
                        "before_prompt_build hook panicked; continuing with previous value"
                    );
                }
            }
        }
        HookResult::Continue(prompt)
    }

    pub async fn run_before_llm_call(
        &self,
        mut messages: Vec<ChatMessage>,
        mut model: String,
    ) -> HookResult<(Vec<ChatMessage>, String)> {
        for h in &self.handlers {
            let hook_name = h.name();
            match AssertUnwindSafe(h.before_llm_call(messages.clone(), model.clone()))
                .catch_unwind()
                .await
            {
                Ok(HookResult::Continue((m, mdl))) => {
                    messages = m;
                    model = mdl;
                }
                Ok(HookResult::Cancel(reason)) => {
                    info!(
                        hook = hook_name,
                        reason, "before_llm_call cancelled by hook"
                    );
                    return HookResult::Cancel(reason);
                }
                Err(_) => {
                    tracing::error!(
                        hook = hook_name,
                        "before_llm_call hook panicked; continuing with previous values"
                    );
                }
            }
        }
        HookResult::Continue((messages, model))
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
                    info!(
                        hook = hook_name,
                        reason, "before_tool_call cancelled by hook"
                    );
                    return HookResult::Cancel(reason);
                }
                Err(_) => {
                    tracing::error!(
                        hook = hook_name,
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
                    info!(
                        hook = hook_name,
                        reason, "on_message_received cancelled by hook"
                    );
                    return HookResult::Cancel(reason);
                }
                Err(_) => {
                    tracing::error!(
                        hook = hook_name,
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
                    info!(
                        hook = hook_name,
                        reason, "on_message_sending cancelled by hook"
                    );
                    return HookResult::Cancel(reason);
                }
                Err(_) => {
                    tracing::error!(
                        hook = hook_name,
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

    // ── C2: Void hook dispatch tests for all Phase 0 hooks ──────

    struct AllEventsHook {
        name: String,
        session_start: Arc<AtomicU32>,
        session_end: Arc<AtomicU32>,
        llm_output: Arc<AtomicU32>,
        message_sent: Arc<AtomicU32>,
        gateway_stop: Arc<AtomicU32>,
        heartbeat: Arc<AtomicU32>,
    }

    impl AllEventsHook {
        fn new(
            name: &str,
        ) -> (
            Self,
            Arc<AtomicU32>,
            Arc<AtomicU32>,
            Arc<AtomicU32>,
            Arc<AtomicU32>,
            Arc<AtomicU32>,
            Arc<AtomicU32>,
        ) {
            let ss = Arc::new(AtomicU32::new(0));
            let se = Arc::new(AtomicU32::new(0));
            let lo = Arc::new(AtomicU32::new(0));
            let ms = Arc::new(AtomicU32::new(0));
            let gs = Arc::new(AtomicU32::new(0));
            let hb = Arc::new(AtomicU32::new(0));
            (
                Self {
                    name: name.to_string(),
                    session_start: ss.clone(),
                    session_end: se.clone(),
                    llm_output: lo.clone(),
                    message_sent: ms.clone(),
                    gateway_stop: gs.clone(),
                    heartbeat: hb.clone(),
                },
                ss,
                se,
                lo,
                ms,
                gs,
                hb,
            )
        }
    }

    #[async_trait]
    impl HookHandler for AllEventsHook {
        fn name(&self) -> &str {
            &self.name
        }
        fn priority(&self) -> i32 {
            0
        }
        async fn on_session_start(&self, _session_id: &str, _channel: &str) {
            self.session_start.fetch_add(1, Ordering::SeqCst);
        }
        async fn on_session_end(&self, _session_id: &str, _channel: &str) {
            self.session_end.fetch_add(1, Ordering::SeqCst);
        }
        async fn on_llm_output(&self, _response: &ChatResponse) {
            self.llm_output.fetch_add(1, Ordering::SeqCst);
        }
        async fn on_message_sent(&self, _channel: &str, _recipient: &str, _content: &str) {
            self.message_sent.fetch_add(1, Ordering::SeqCst);
        }
        async fn on_gateway_stop(&self) {
            self.gateway_stop.fetch_add(1, Ordering::SeqCst);
        }
        async fn on_heartbeat_tick(&self) {
            self.heartbeat.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn fire_session_start_dispatches_to_all_handlers() {
        let mut runner = HookRunner::new();
        let (hook, ss, _, _, _, _, _) = AllEventsHook::new("test");
        runner.register(Box::new(hook));

        runner.fire_session_start("sess-1", "telegram").await;

        assert_eq!(ss.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fire_session_end_dispatches_to_all_handlers() {
        let mut runner = HookRunner::new();
        let (hook, _, se, _, _, _, _) = AllEventsHook::new("test");
        runner.register(Box::new(hook));

        runner.fire_session_end("sess-1", "telegram").await;

        assert_eq!(se.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fire_llm_output_dispatches_to_all_handlers() {
        let mut runner = HookRunner::new();
        let (hook, _, _, lo, _, _, _) = AllEventsHook::new("test");
        runner.register(Box::new(hook));

        let response = ChatResponse {
            text: Some("Hello".to_string()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        };
        runner.fire_llm_output(&response).await;

        assert_eq!(lo.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fire_message_sent_dispatches_to_all_handlers() {
        let mut runner = HookRunner::new();
        let (hook, _, _, _, ms, _, _) = AllEventsHook::new("test");
        runner.register(Box::new(hook));

        runner
            .fire_message_sent("telegram", "user123", "Reply text")
            .await;

        assert_eq!(ms.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fire_gateway_stop_dispatches_to_all_handlers() {
        let mut runner = HookRunner::new();
        let (hook, _, _, _, _, gs, _) = AllEventsHook::new("test");
        runner.register(Box::new(hook));

        runner.fire_gateway_stop().await;

        assert_eq!(gs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn all_phase0_hooks_fire_independently() {
        let mut runner = HookRunner::new();
        let (hook, ss, se, lo, ms, gs, hb) = AllEventsHook::new("all-events");
        runner.register(Box::new(hook));

        runner.fire_session_start("s1", "discord").await;
        runner.fire_session_end("s1", "discord").await;
        let response = ChatResponse {
            text: Some("test".to_string()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        };
        runner.fire_llm_output(&response).await;
        runner.fire_message_sent("discord", "user", "hi").await;
        runner.fire_gateway_stop().await;
        runner.fire_heartbeat_tick().await;

        assert_eq!(ss.load(Ordering::SeqCst), 1, "session_start");
        assert_eq!(se.load(Ordering::SeqCst), 1, "session_end");
        assert_eq!(lo.load(Ordering::SeqCst), 1, "llm_output");
        assert_eq!(ms.load(Ordering::SeqCst), 1, "message_sent");
        assert_eq!(gs.load(Ordering::SeqCst), 1, "gateway_stop");
        assert_eq!(hb.load(Ordering::SeqCst), 1, "heartbeat_tick");
    }
}
