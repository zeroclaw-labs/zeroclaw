//! Per-iteration tool-spec assembly for the turn engine.

use crate::tools::{ActivatedToolSet, Tool, ToolSpec};
use anyhow::Result;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use zeroclaw_providers::ModelProvider;

/// Tool specs assembled for one loop iteration.
pub(crate) struct IterationToolSpecs {
    pub(crate) tool_specs: Vec<ToolSpec>,
    pub(crate) known_tool_names: HashSet<String>,
    pub(crate) use_native_tools: bool,
}

impl IterationToolSpecs {
    pub(crate) fn refresh_native_tool_mode(&mut self, model_provider: &dyn ModelProvider) {
        self.use_native_tools =
            model_provider.supports_native_tools() && !self.tool_specs.is_empty();
    }

    /// Construct iteration tool specs from a per-request override, bypassing
    /// the normal `tools_registry` → `tool.spec()` rebuild.
    ///
    /// Applies `excluded_tools` filtering to honour security policy — tools
    /// excluded by policy are removed even if present in the override.
    pub(crate) fn from_override(
        model_provider: &dyn ModelProvider,
        override_specs: &[ToolSpec],
        excluded_tools: &[String],
    ) -> Self {
        let tool_specs: Vec<ToolSpec> = override_specs
            .iter()
            .filter(|s| !excluded_tools.iter().any(|ex| ex == &s.name))
            .cloned()
            .collect();
        let known_tool_names: HashSet<String> = tool_specs
            .iter()
            .map(|s| s.name.to_ascii_lowercase())
            .collect();
        let use_native_tools = model_provider.supports_native_tools() && !tool_specs.is_empty();
        IterationToolSpecs {
            tool_specs,
            known_tool_names,
            use_native_tools,
        }
    }
}

pub(crate) fn build_iteration_tool_specs(
    model_provider: &dyn ModelProvider,
    tools_registry: &[Box<dyn Tool>],
    excluded_tools: &[String],
    activated_tools: Option<&Arc<Mutex<ActivatedToolSet>>>,
) -> Result<IterationToolSpecs> {
    // Rebuild tool_specs each iteration so newly activated deferred tools appear.
    let mut tool_specs: Vec<crate::tools::ToolSpec> = tools_registry
        .iter()
        .filter(|tool| !excluded_tools.iter().any(|ex| ex == tool.name()))
        .map(|tool| tool.spec())
        .collect();
    if let Some(at) = activated_tools {
        let activated_tools = match at.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_category(::zeroclaw_log::EventCategory::Tool)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "activated-tool lock poisoned while assembling iteration tool specs; recovering guard for read"
                );
                poisoned.into_inner()
            }
        };
        for spec in activated_tools.tool_specs() {
            if !excluded_tools.iter().any(|ex| ex == &spec.name) {
                tool_specs.push(spec);
            }
        }
    }
    let known_tool_names: HashSet<String> = tool_specs
        .iter()
        .map(|tool| tool.name.to_ascii_lowercase())
        .collect();
    let use_native_tools = model_provider.supports_native_tools() && !tool_specs.is_empty();

    Ok(IterationToolSpecs {
        tool_specs,
        known_tool_names,
        use_native_tools,
    })
}

#[cfg(test)]
mod tests {
    use super::{IterationToolSpecs, build_iteration_tool_specs};
    use crate::tools::{ActivatedToolSet, ToolSpec};
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use zeroclaw_api::attribution::Role;
    use zeroclaw_api::model_provider::{ModelProvider, ProviderCapabilities};
    use zeroclaw_api::tool::Tool;

    struct NativeToolsProvider;

    #[async_trait]
    impl ModelProvider for NativeToolsProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: true,
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
            unreachable!("test provider should not execute chat")
        }
    }

    impl zeroclaw_api::attribution::Attributable for NativeToolsProvider {
        fn role(&self) -> Role {
            Role::System
        }

        fn alias(&self) -> &str {
            "test-native-tools-provider"
        }
    }

    struct PromptToolsProvider;

    #[async_trait]
    impl ModelProvider for PromptToolsProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: false,
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
            unreachable!("test provider should not execute chat")
        }
    }

    impl zeroclaw_api::attribution::Attributable for PromptToolsProvider {
        fn role(&self) -> Role {
            Role::System
        }

        fn alias(&self) -> &str {
            "test-prompt-tools-provider"
        }
    }

    struct CountingTool {
        name: String,
        invocations: Arc<AtomicUsize>,
    }

    impl CountingTool {
        fn new(name: &str, invocations: Arc<AtomicUsize>) -> Self {
            Self {
                name: name.to_string(),
                invocations,
            }
        }
    }

    #[async_trait]
    impl Tool for CountingTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Counts executions for poisoned-lock tests"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                }
            })
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            Ok(crate::tools::ToolResult {
                success: true,
                output: "counted".into(),
                error: None,
            })
        }
    }

    impl zeroclaw_api::attribution::Attributable for CountingTool {
        fn role(&self) -> Role {
            Role::Tool(zeroclaw_api::attribution::ToolKind::Plugin)
        }

        fn alias(&self) -> &str {
            self.name()
        }
    }

    #[test]
    fn build_iteration_tool_specs_recovers_poisoned_activated_tool_lock() {
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let invocations = Arc::new(AtomicUsize::new(0));
        let activated_tool: Arc<dyn Tool> = Arc::new(CountingTool::new(
            "docker-mcp__extract_text",
            Arc::clone(&invocations),
        ));
        activated
            .lock()
            .unwrap()
            .activate("docker-mcp__extract_text".into(), activated_tool);
        let poisoned = Arc::clone(&activated);
        let _ = std::thread::spawn(move || {
            let _guard = poisoned.lock().expect("test mutex should lock");
            panic!("poison activated-tools lock");
        })
        .join();

        let specs = build_iteration_tool_specs(&NativeToolsProvider, &[], &[], Some(&activated))
            .expect("poisoned activated-tools lock should recover for read");
        assert!(
            specs
                .tool_specs
                .iter()
                .any(|spec| spec.name == "docker-mcp__extract_text"),
            "recovered poisoned lock should still expose activated tool specs"
        );
    }

    #[test]
    fn iteration_tool_specs_recomputes_native_mode_for_active_provider() {
        let invocations = Arc::new(AtomicUsize::new(0));
        let tool = Box::new(CountingTool::new("read_file", invocations));
        let mut specs = build_iteration_tool_specs(&NativeToolsProvider, &[tool], &[], None)
            .expect("native provider with tools should build specs");
        assert!(specs.use_native_tools);

        specs.refresh_native_tool_mode(&PromptToolsProvider);

        assert!(
            !specs.use_native_tools,
            "active provider must decide whether this turn uses native tool transport"
        );
    }

    // ── Regression: default rebuild path includes activated tools ──────

    #[test]
    fn build_specs_includes_activated_tools_with_mcp_suffixes() {
        // When no TOOL_SPECS_OVERRIDE is set, the per-iteration rebuild must
        // surface tools activated at runtime — including those with MCP-style
        // prefixed names (docker-mcp__extract_text). The `from_override()`
        // snapshot path must NOT be used here; it would miss these tools.
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let invocations = Arc::new(AtomicUsize::new(0));
        let activated_tool: Arc<dyn Tool> = Arc::new(CountingTool::new(
            "docker-mcp__extract_text",
            Arc::clone(&invocations),
        ));
        activated
            .lock()
            .unwrap()
            .activate("docker-mcp__extract_text".into(), activated_tool);

        let specs = build_iteration_tool_specs(&NativeToolsProvider, &[], &[], Some(&activated))
            .expect("build should succeed with activated tools");

        assert!(
            specs.known_tool_names.contains("docker-mcp__extract_text"),
            "default rebuild must include MCP-suffix activated tools"
        );
        assert!(
            specs
                .tool_specs
                .iter()
                .any(|s| s.name == "docker-mcp__extract_text"),
            "default rebuild tool_specs must list the activated MCP tool"
        );
    }

    // ── Regression: override path is a snapshot, not a live rebuild ─────

    #[test]
    fn from_override_does_not_include_activated_tools() {
        // `from_override` takes a pre-built Vec<ToolSpec> — it must NOT
        // consult the activated-tool set. The override is a snapshot frozen
        // at request time (chat-completions tools parameter).
        let activated = Arc::new(Mutex::new(ActivatedToolSet::new()));
        let invocations = Arc::new(AtomicUsize::new(0));
        let activated_tool: Arc<dyn Tool> =
            Arc::new(CountingTool::new("extra_tool", Arc::clone(&invocations)));
        activated
            .lock()
            .unwrap()
            .activate("extra_tool".into(), activated_tool);

        let override_specs = vec![ToolSpec::new("weather_query", "Query weather", json!({}))];
        let specs = IterationToolSpecs::from_override(&NativeToolsProvider, &override_specs, &[]);

        assert_eq!(specs.tool_specs.len(), 1);
        assert_eq!(specs.tool_specs[0].name, "weather_query");
        assert!(
            !specs.known_tool_names.contains("extra_tool"),
            "override must not include activated tools — it is a snapshot"
        );
    }
}
