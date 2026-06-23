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
    use super::build_iteration_tool_specs;
    use crate::tools::ActivatedToolSet;
    use async_trait::async_trait;
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
}
