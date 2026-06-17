//! Per-iteration tool-spec assembly for the turn engine.

use crate::tools::{ActivatedToolSet, Tool, ToolSpec};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use zeroclaw_providers::ModelProvider;

/// Tool specs assembled for one loop iteration.
pub(crate) struct IterationToolSpecs {
    pub(crate) tool_specs: Vec<ToolSpec>,
    pub(crate) known_tool_names: HashSet<String>,
    pub(crate) use_native_tools: bool,
}

pub(crate) fn build_iteration_tool_specs(
    model_provider: &dyn ModelProvider,
    tools_registry: &[Box<dyn Tool>],
    excluded_tools: &[String],
    activated_tools: Option<&Arc<Mutex<ActivatedToolSet>>>,
) -> IterationToolSpecs {
    // Rebuild tool_specs each iteration so newly activated deferred tools appear.
    let mut tool_specs: Vec<crate::tools::ToolSpec> = tools_registry
        .iter()
        .filter(|tool| !excluded_tools.iter().any(|ex| ex == tool.name()))
        .map(|tool| tool.spec())
        .collect();
    if let Some(at) = activated_tools {
        for spec in at.lock().unwrap_or_else(|e| e.into_inner()).tool_specs() {
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

    IterationToolSpecs {
        tool_specs,
        known_tool_names,
        use_native_tools,
    }
}
