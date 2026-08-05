//! Blueprint graph projection of a `Sop`.
//!
//! Projects the linear step list plus routing metadata into a node/wire
//! graph for visual editors (web node canvas, zerocode SOP pane) and text
//! renderers. Pure projection: building a graph never mutates the SOP.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use zeroclaw_api::tool::ToolSpec;

use super::binding::{BindingRef, BindingScope, ExtractedBinding, extract_bindings_with_paths};
use super::types::{Sop, SopStep};

pub type ToolSpecs = HashMap<String, ToolSpec>;

// Graph wire shape lives in the leaf crate `zeroclaw-sop-graph` so the
// runtime projection, the gateway JSON Schema, and the zerocode TUI all read
// one definition. Re-exported here so the projection logic below and every
// downstream `crate::sop::graph::GraphPin` path keep working unchanged.
pub use zeroclaw_sop_graph::{
    FlowRole, GraphDiagnostic, GraphLayout, GraphLegend, GraphNode, GraphPin, GraphSeverity,
    GraphWire, LayoutGeometry, LegendEntry, NodeKind, NodePosition, NodeRunState, PinClass,
    SopGraph, TRIGGER_NODE_BASE,
};

/// Rendering style for `render_graph_text`.
pub enum TextGraphFormat {
    /// Numbered step list with flow successors (`1. Title -> 2, 3`).
    Outline,
    /// One edge per line with a role label (`1 -> 2 [switch:pr]`).
    Adjacency,
    /// Pretty-printed JSON of the whole `SopGraph`.
    Json,
}

/// Render a graph as plain text for CLI output and agent-readable summaries.
/// Diagnostics are appended as a trailing block in non-JSON formats.
pub fn render_graph_text(graph: &SopGraph, format: &TextGraphFormat) -> String {
    match format {
        TextGraphFormat::Json => {
            serde_json::to_string_pretty(graph).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
        }
        TextGraphFormat::Adjacency => {
            let mut out = String::new();
            for wire in &graph.wires {
                let label = match (wire.class, wire.flow_role, &wire.from_pin) {
                    (PinClass::Flow, Some(FlowRole::Switch), Some(port)) => {
                        let role: &'static str = FlowRole::Switch.into();
                        format!("{role}:{port}")
                    }
                    (PinClass::Flow, Some(role), _) => {
                        let role: &'static str = role.into();
                        role.to_string()
                    }
                    (class, _, _) => {
                        let class: &'static str = class.into();
                        class.to_string()
                    }
                };
                out.push_str(&format!(
                    "{} -> {} [{}]\n",
                    wire.from_step, wire.to_step, label
                ));
            }
            append_diagnostics(graph, &mut out);
            out
        }
        TextGraphFormat::Outline => {
            let mut out = String::new();
            for node in &graph.nodes {
                let outs: Vec<String> = graph
                    .wires
                    .iter()
                    .filter(|w| w.from_step == node.step && w.class == PinClass::Flow)
                    .map(|w| w.to_step.to_string())
                    .collect();
                if outs.is_empty() {
                    out.push_str(&format!("{}. {}\n", node.step, node.title));
                } else {
                    out.push_str(&format!(
                        "{}. {} -> {}\n",
                        node.step,
                        node.title,
                        outs.join(", ")
                    ));
                }
            }
            append_diagnostics(graph, &mut out);
            out
        }
    }
}

fn append_diagnostics(graph: &SopGraph, out: &mut String) {
    if graph.diagnostics.is_empty() {
        return;
    }
    out.push_str("\ndiagnostics:\n");
    for diag in &graph.diagnostics {
        let sev: &'static str = diag.severity.into();
        out.push_str(&format!(
            "  [{}] step {}: {}\n",
            sev, diag.step, diag.message
        ));
    }
}

fn schema_type(fragment: &serde_json::Value) -> Option<String> {
    match fragment {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(map) => {
            map.get("type").and_then(|t| t.as_str()).map(str::to_string)
        }
        _ => None,
    }
}

fn schema_required(fragment: &serde_json::Value) -> bool {
    match fragment {
        serde_json::Value::Object(map) => map
            .get("required")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        _ => true,
    }
}

fn schema_data_pin(fragment: Option<&serde_json::Value>, name: &str) -> Option<GraphPin> {
    fragment.map(|fragment| GraphPin {
        class: PinClass::Data,
        name: name.to_string(),
        data_type: schema_type(fragment),
        required: schema_required(fragment),
    })
}

fn object_field_pins(schema: &serde_json::Value, prefix: &str) -> Vec<GraphPin> {
    let serde_json::Value::Object(map) = schema else {
        return Vec::new();
    };
    let Some(serde_json::Value::Object(props)) = map.get("properties") else {
        return schema_data_pin(Some(schema), prefix).into_iter().collect();
    };
    let required: std::collections::HashSet<&str> = map
        .get("required")
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    props
        .iter()
        .map(|(field, frag)| GraphPin {
            class: PinClass::Data,
            name: format!("{prefix}.{field}"),
            data_type: schema_type(frag),
            required: required.contains(field.as_str()),
        })
        .collect()
}

fn call_data_pins(step: &SopStep, specs: &ToolSpecs) -> (Vec<GraphPin>, Vec<GraphPin>) {
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    for (idx, call) in step.calls.iter().enumerate() {
        let prefix = format!("calls.{idx}");
        let Some(spec) = specs.get(&call.tool) else {
            continue;
        };
        inputs.extend(object_field_pins(&spec.parameters, &prefix));
        if let Some(output) = &spec.output {
            outputs.push(GraphPin {
                class: PinClass::Data,
                name: prefix,
                data_type: schema_type(output),
                required: false,
            });
        }
    }
    (inputs, outputs)
}

fn node_for(step: &SopStep, specs: &ToolSpecs) -> GraphNode {
    let mut inputs = vec![GraphPin {
        class: PinClass::Flow,
        name: "in".to_string(),
        data_type: None,
        required: false,
    }];
    let mut outputs = if step.routing.switch.is_empty() {
        vec![GraphPin {
            class: PinClass::Flow,
            name: "out".to_string(),
            data_type: None,
            required: false,
        }]
    } else {
        step.routing
            .switch
            .iter()
            .map(|rule| GraphPin {
                class: PinClass::Flow,
                name: rule.name.clone(),
                data_type: None,
                required: false,
            })
            .collect()
    };

    let (call_inputs, call_outputs) = call_data_pins(step, specs);
    inputs.extend(call_inputs);
    outputs.extend(call_outputs);

    if let Some(schema) = &step.schema {
        inputs.extend(schema_data_pin(schema.input.as_ref(), "input"));
        outputs.extend(schema_data_pin(schema.output.as_ref(), "output"));
    }

    GraphNode {
        step: step.number,
        title: step.title.clone(),
        kind: NodeKind::Step,
        subtitle: None,
        trigger_index: None,
        inputs,
        outputs,
    }
}

fn trigger_node(index: usize, trigger: &super::types::SopTrigger) -> GraphNode {
    let (title, subtitle) = trigger_labels(trigger);
    GraphNode {
        step: TRIGGER_NODE_BASE + index as u32,
        title,
        kind: NodeKind::Trigger,
        subtitle: Some(subtitle),
        trigger_index: Some(index as u32),
        inputs: Vec::new(),
        outputs: vec![GraphPin {
            class: PinClass::Flow,
            name: "event".to_string(),
            data_type: None,
            required: false,
        }],
    }
}

fn trigger_labels(trigger: &super::types::SopTrigger) -> (String, String) {
    let kind = match trigger {
        super::types::SopTrigger::Channel { channel, .. } => channel.clone(),
        other => other.source().to_string(),
    };
    (kind, trigger.to_string())
}

fn types_compatible(from: Option<&str>, to: Option<&str>) -> bool {
    match (from, to) {
        (None, _) | (_, None) => true,
        (Some(a), Some(b)) => a == b,
    }
}

fn binding_matches_output(path: &str, pin_name: &str) -> bool {
    path == pin_name
        || path
            .strip_prefix(pin_name)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// Projection constructors for the shared `SopGraph` type. `SopGraph` lives
/// in `zeroclaw-sop-graph`, so the build logic that needs the runtime's
/// `Sop`/`ToolSpec` hangs off this extension trait instead of an inherent
/// impl. Call sites keep using `SopGraph::from_sop(..)` with the trait in
/// scope.
pub trait SopGraphExt {
    /// Project a SOP into a graph. Never fails: unresolvable references
    /// (missing steps, dangling switch ports, unsatisfied required inputs)
    /// become diagnostics instead of errors, so editors can render and fix
    /// broken drafts.
    fn from_sop(sop: &Sop) -> Self;

    fn from_sop_with_specs(sop: &Sop, specs: &ToolSpecs) -> Self;

    /// True when any diagnostic is an error. Errors block `validate_sop_strict`.
    fn has_errors(&self) -> bool;
}

impl SopGraphExt for SopGraph {
    fn from_sop(sop: &Sop) -> Self {
        Self::from_sop_with_specs(sop, &ToolSpecs::new())
    }

    fn from_sop_with_specs(sop: &Sop, specs: &ToolSpecs) -> Self {
        let mut nodes: Vec<GraphNode> = sop.steps.iter().map(|s| node_for(s, specs)).collect();
        let valid_steps: std::collections::HashSet<u32> =
            sop.steps.iter().map(|s| s.number).collect();

        let mut wires = Vec::new();
        let mut diagnostics = Vec::new();

        for (idx, step) in sop.steps.iter().enumerate() {
            let switch_supersedes = !step.routing.switch.is_empty();
            match step.routing.next {
                Some(next) if !switch_supersedes => {
                    if valid_steps.contains(&next) {
                        wires.push(GraphWire {
                            class: PinClass::Flow,
                            from_step: step.number,
                            to_step: next,
                            flow_role: Some(FlowRole::Sequence),
                            from_pin: None,
                            to_pin: None,
                        });
                    } else {
                        diagnostics.push(GraphDiagnostic {
                            severity: GraphSeverity::Error,
                            step: step.number,
                            message: format!("next target step {next} does not exist"),
                        });
                    }
                }
                None if !switch_supersedes => {
                    if !step.routing.terminal
                        && let Some(following) = sop.steps.get(idx + 1)
                    {
                        wires.push(GraphWire {
                            class: PinClass::Flow,
                            from_step: step.number,
                            to_step: following.number,
                            flow_role: Some(FlowRole::Sequence),
                            from_pin: None,
                            to_pin: None,
                        });
                    }
                }
                _ => {}
            }

            for dep in &step.routing.depends_on {
                if valid_steps.contains(dep) {
                    wires.push(GraphWire {
                        class: PinClass::Flow,
                        from_step: *dep,
                        to_step: step.number,
                        flow_role: Some(FlowRole::Dependency),
                        from_pin: None,
                        to_pin: None,
                    });
                } else {
                    diagnostics.push(GraphDiagnostic {
                        severity: GraphSeverity::Error,
                        step: step.number,
                        message: format!("depends_on target step {dep} does not exist"),
                    });
                }
            }

            if let super::step_contract::StepFailure::Goto { step: target } = &step.on_failure {
                if valid_steps.contains(target) {
                    wires.push(GraphWire {
                        class: PinClass::Flow,
                        from_step: step.number,
                        to_step: *target,
                        flow_role: Some(FlowRole::Failure),
                        from_pin: None,
                        to_pin: None,
                    });
                } else {
                    diagnostics.push(GraphDiagnostic {
                        severity: GraphSeverity::Error,
                        step: step.number,
                        message: format!("on_failure goto target step {target} does not exist"),
                    });
                }
            }

            if !step.routing.switch.is_empty() && step.routing.next.is_some() {
                diagnostics.push(GraphDiagnostic {
                    severity: GraphSeverity::Warning,
                    step: step.number,
                    message:
                        "step has switch rules and a routing.next target; next is ignored because \
                         switch resolution takes precedence"
                            .to_string(),
                });
            }

            for rule in &step.routing.switch {
                match rule.goto {
                    Some(target) if valid_steps.contains(&target) => {
                        wires.push(GraphWire {
                            class: PinClass::Flow,
                            from_step: step.number,
                            to_step: target,
                            flow_role: Some(FlowRole::Switch),
                            from_pin: Some(rule.name.clone()),
                            to_pin: None,
                        });
                    }
                    Some(target) => {
                        diagnostics.push(GraphDiagnostic {
                            severity: GraphSeverity::Error,
                            step: step.number,
                            message: format!(
                                "switch port '{}' target step {target} does not exist",
                                rule.name
                            ),
                        });
                    }
                    None => {
                        diagnostics.push(GraphDiagnostic {
                            severity: GraphSeverity::Warning,
                            step: step.number,
                            message: format!("switch port '{}' has no target", rule.name),
                        });
                    }
                }
            }
        }

        let has_inbound: std::collections::HashSet<u32> = wires
            .iter()
            .filter(|w| w.class == PinClass::Flow)
            .map(|w| w.to_step)
            .collect();
        let entry_steps: Vec<u32> = sop
            .steps
            .iter()
            .map(|s| s.number)
            .filter(|n| !has_inbound.contains(n))
            .collect();
        let entry_steps = if entry_steps.is_empty() {
            sop.steps.first().map(|s| s.number).into_iter().collect()
        } else {
            entry_steps
        };
        for (index, trigger) in sop.triggers.iter().enumerate() {
            let node = trigger_node(index, trigger);
            let source = node.step;
            nodes.push(node);
            for entry in &entry_steps {
                wires.push(GraphWire {
                    class: PinClass::Flow,
                    from_step: source,
                    to_step: *entry,
                    flow_role: Some(FlowRole::Trigger),
                    from_pin: Some("event".to_string()),
                    to_pin: None,
                });
            }
        }

        binding_data_wires(sop, &nodes, &mut wires, &mut diagnostics);

        let mut layout = layout_graph(&nodes, &wires);
        for step in &sop.steps {
            if let Some(p) = step.pos
                && let Some(np) = layout
                    .positions
                    .iter_mut()
                    .find(|np| np.step == step.number)
            {
                np.x = Some(p.x);
                np.y = Some(p.y);
            }
        }
        SopGraph {
            nodes,
            wires,
            diagnostics,
            layout,
        }
    }

    fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == GraphSeverity::Error)
    }
}

fn layout_graph(nodes: &[GraphNode], wires: &[GraphWire]) -> GraphLayout {
    use std::collections::HashMap;

    let mut preds: HashMap<u32, Vec<u32>> = HashMap::new();
    for node in nodes {
        preds.entry(node.step).or_default();
    }
    for wire in wires.iter().filter(|w| w.class == PinClass::Flow) {
        preds.entry(wire.to_step).or_default().push(wire.from_step);
    }

    let mut col: HashMap<u32, u32> = HashMap::new();
    fn resolve(
        step: u32,
        preds: &std::collections::HashMap<u32, Vec<u32>>,
        col: &mut std::collections::HashMap<u32, u32>,
        seen: &mut std::collections::HashSet<u32>,
    ) -> u32 {
        if let Some(c) = col.get(&step) {
            return *c;
        }
        if !seen.insert(step) {
            return 0;
        }
        let parents = preds.get(&step).cloned().unwrap_or_default();
        let c = parents
            .iter()
            .map(|p| resolve(*p, preds, col, seen) + 1)
            .max()
            .unwrap_or(0);
        seen.remove(&step);
        col.insert(step, c);
        c
    }

    let mut ordered: Vec<u32> = nodes.iter().map(|n| n.step).collect();
    ordered.sort_unstable();
    for step in &ordered {
        let mut seen = std::collections::HashSet::new();
        resolve(*step, &preds, &mut col, &mut seen);
    }

    let mut row_by_col: HashMap<u32, u32> = HashMap::new();
    let mut positions = Vec::with_capacity(nodes.len());
    let mut columns = 0u32;
    let mut rows = 0u32;
    for step in &ordered {
        let c = col.get(step).copied().unwrap_or(0);
        let r = row_by_col.entry(c).or_insert(0);
        positions.push(NodePosition {
            step: *step,
            col: c,
            row: *r,
            x: None,
            y: None,
        });
        columns = columns.max(c + 1);
        rows = rows.max(*r + 1);
        *r += 1;
    }

    GraphLayout {
        positions,
        columns,
        rows,
        ..GraphLayout::default()
    }
}

fn binding_data_wires(
    sop: &Sop,
    nodes: &[GraphNode],
    wires: &mut Vec<GraphWire>,
    diagnostics: &mut Vec<GraphDiagnostic>,
) {
    let node_by_step: HashMap<u32, &GraphNode> = nodes.iter().map(|n| (n.step, n)).collect();

    for step in &sop.steps {
        let consumer = node_by_step.get(&step.number);
        for (call_idx, call) in step.calls.iter().enumerate() {
            for (arg_path, extracted) in extract_bindings_with_paths(&call.args) {
                let binding = match extracted {
                    ExtractedBinding::Valid(binding) => binding,
                    ExtractedBinding::Malformed { raw, reason } => {
                        diagnostics.push(GraphDiagnostic {
                            severity: GraphSeverity::Error,
                            step: step.number,
                            message: format!("malformed binding `{raw}`: {reason}"),
                        });
                        continue;
                    }
                };
                let BindingScope::Step(producer_step) = binding.scope else {
                    continue;
                };
                let to_pin = if arg_path.is_empty() {
                    format!("calls.{call_idx}")
                } else {
                    format!("calls.{call_idx}.{arg_path}")
                };
                wire_binding(
                    &node_by_step,
                    consumer.copied(),
                    step.number,
                    producer_step,
                    &binding,
                    &to_pin,
                    wires,
                    diagnostics,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn wire_binding(
    node_by_step: &HashMap<u32, &GraphNode>,
    consumer: Option<&GraphNode>,
    consumer_step: u32,
    producer_step: u32,
    binding: &BindingRef,
    to_pin: &str,
    wires: &mut Vec<GraphWire>,
    diagnostics: &mut Vec<GraphDiagnostic>,
) {
    let Some(producer) = node_by_step.get(&producer_step) else {
        diagnostics.push(GraphDiagnostic {
            severity: GraphSeverity::Error,
            step: consumer_step,
            message: format!("binding references step {producer_step} which does not exist"),
        });
        return;
    };
    let from_pin = producer
        .outputs
        .iter()
        .filter(|p| p.class == PinClass::Data)
        .find(|p| binding_matches_output(&binding.path, &p.name))
        .or_else(|| producer.outputs.iter().find(|p| p.class == PinClass::Data));
    let (from_pin_name, from_type) = match from_pin {
        Some(pin) => (pin.name.clone(), pin.data_type.as_deref()),
        None => (format!("steps.{producer_step}"), None),
    };
    let to_type = consumer
        .and_then(|c| c.inputs.iter().find(|p| p.name == to_pin))
        .and_then(|p| p.data_type.as_deref());
    if !types_compatible(from_type, to_type) {
        diagnostics.push(GraphDiagnostic {
            severity: GraphSeverity::Error,
            step: consumer_step,
            message: format!(
                "data type mismatch: step {} output `{}` ({}) does not satisfy input `{}` ({})",
                producer_step,
                from_pin_name,
                from_type.unwrap_or("any"),
                to_pin,
                to_type.unwrap_or("any"),
            ),
        });
        return;
    }
    wires.push(GraphWire {
        class: PinClass::Data,
        from_step: producer_step,
        to_step: consumer_step,
        flow_role: None,
        from_pin: Some(from_pin_name),
        to_pin: Some(to_pin.to_string()),
    });
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// Run state for one step node. Trigger nodes carry no run state and are
/// omitted from overlays.
pub struct NodeRunOverlay {
    pub step: u32,
    pub state: NodeRunState,
    /// Tool invocations captured while the step executed. Empty for
    /// unreached steps and runs recorded before capture landed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<super::types::StepToolCall>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// Live run state layered over a `SopGraph`, letting a canvas animate an
/// execution without re-fetching the graph. Served by `sops/run-overlay`.
pub struct RunOverlay {
    pub run_id: String,
    pub sop_name: String,
    pub status: super::types::SopRunStatus,
    pub current_step: u32,
    pub total_steps: u32,
    pub waiting: bool,
    pub paused: bool,
    pub nodes: Vec<NodeRunOverlay>,
}

impl RunOverlay {
    /// Project a run onto a graph. Recorded step results win; the current
    /// step shows `Active` only while the run is non-terminal.
    pub fn project(graph: &SopGraph, run: &super::types::SopRun) -> Self {
        use super::types::{SopRunStatus, SopStepStatus};

        let terminal_run = matches!(
            run.status,
            SopRunStatus::Completed | SopRunStatus::Failed | SopRunStatus::Cancelled
        );
        let waiting = run.status == SopRunStatus::WaitingApproval;
        let paused = run.status == SopRunStatus::PausedCheckpoint;

        let nodes = graph
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Step)
            .map(|node| {
                let recorded = run.step_results.iter().find(|r| r.step_number == node.step);
                let state = match recorded.map(|r| match r.status {
                    SopStepStatus::Completed => NodeRunState::Completed,
                    SopStepStatus::Failed => NodeRunState::Failed,
                    SopStepStatus::Skipped => NodeRunState::Skipped,
                }) {
                    Some(s) => s,
                    None if !terminal_run && node.step == run.current_step => NodeRunState::Active,
                    None => NodeRunState::Pending,
                };
                NodeRunOverlay {
                    step: node.step,
                    state,
                    tool_calls: recorded.map(|r| r.tool_calls.clone()).unwrap_or_default(),
                }
            })
            .collect();

        Self {
            run_id: run.run_id.clone(),
            sop_name: run.sop_name.clone(),
            status: run.status,
            current_step: run.current_step,
            total_steps: run.total_steps,
            waiting,
            paused,
            nodes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sop::step_contract::{StepFailure, SwitchRule};
    use crate::sop::types::{
        Sop, SopEvent, SopExecutionMode, SopPriority, SopRun, SopRunStatus, SopStepResult,
        SopStepStatus, SopTrigger, SopTriggerSource, StepSchema,
    };

    fn step(number: u32, title: &str) -> SopStep {
        SopStep {
            number,
            title: title.to_string(),
            ..SopStep::default()
        }
    }

    fn sop(steps: Vec<SopStep>) -> Sop {
        Sop {
            name: "g".into(),
            description: String::new(),
            version: "0.1.0".into(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Auto,
            triggers: Vec::new(),
            steps,
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
            admission_policy: Default::default(),
            max_pending_approvals: 0,
            agent: None,
        }
    }

    fn flow_wires(graph: &SopGraph, role: FlowRole) -> Vec<(u32, u32)> {
        graph
            .wires
            .iter()
            .filter(|w| w.flow_role == Some(role))
            .map(|w| (w.from_step, w.to_step))
            .collect()
    }

    #[test]
    fn linear_steps_get_implicit_sequence_wires() {
        let graph = SopGraph::from_sop(&sop(vec![step(1, "a"), step(2, "b"), step(3, "c")]));
        assert_eq!(flow_wires(&graph, FlowRole::Sequence), vec![(1, 2), (2, 3)]);
        assert!(graph.diagnostics.is_empty());
    }

    #[test]
    fn terminal_step_suppresses_fallthrough() {
        let mut s1 = step(1, "a");
        s1.routing.terminal = true;
        let graph = SopGraph::from_sop(&sop(vec![s1, step(2, "b")]));
        assert!(flow_wires(&graph, FlowRole::Sequence).is_empty());
    }

    #[test]
    fn explicit_next_overrides_fallthrough_and_missing_target_is_error() {
        let mut s1 = step(1, "a");
        s1.routing.next = Some(3);
        let mut s2 = step(2, "b");
        s2.routing.next = Some(9);
        let graph = SopGraph::from_sop(&sop(vec![s1, s2, step(3, "c")]));
        assert_eq!(flow_wires(&graph, FlowRole::Sequence), vec![(1, 3)]);
        assert!(graph.has_errors());
        assert!(graph.diagnostics[0].message.contains("step 9"));
    }

    #[test]
    fn depends_on_produces_dependency_wire_and_bad_dep_is_error() {
        let mut s2 = step(2, "b");
        s2.routing.depends_on = vec![1, 7];
        let graph = SopGraph::from_sop(&sop(vec![step(1, "a"), s2]));
        assert_eq!(flow_wires(&graph, FlowRole::Dependency), vec![(1, 2)]);
        assert!(graph.has_errors());
    }

    #[test]
    fn on_failure_goto_produces_failure_wire() {
        let mut s1 = step(1, "a");
        s1.on_failure = StepFailure::Goto { step: 2 };
        let graph = SopGraph::from_sop(&sop(vec![s1, step(2, "b")]));
        assert_eq!(flow_wires(&graph, FlowRole::Failure), vec![(1, 2)]);
    }

    #[test]
    fn switch_ports_replace_default_out_pin_and_carry_port_name() {
        let mut s1 = step(1, "a");
        s1.routing.switch = vec![
            SwitchRule {
                name: "pr".into(),
                when: Some("$.event == \"pull_request\"".into()),
                goto: Some(2),
            },
            SwitchRule {
                name: "dangling".into(),
                when: None,
                goto: None,
            },
            SwitchRule {
                name: "bad".into(),
                when: None,
                goto: Some(9),
            },
        ];
        let graph = SopGraph::from_sop(&sop(vec![s1, step(2, "b")]));

        let node = graph.nodes.iter().find(|n| n.step == 1).unwrap();
        let out_names: Vec<&str> = node.outputs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(out_names, vec!["pr", "dangling", "bad"]);

        let switch: Vec<_> = graph
            .wires
            .iter()
            .filter(|w| w.flow_role == Some(FlowRole::Switch))
            .collect();
        assert_eq!(switch.len(), 1);
        assert_eq!(switch[0].from_pin.as_deref(), Some("pr"));

        assert!(
            flow_wires(&graph, FlowRole::Sequence).is_empty(),
            "switch resolution supersedes sequence fallthrough; no sequence wire should be emitted"
        );

        assert!(
            graph
                .diagnostics
                .iter()
                .any(|d| d.severity == GraphSeverity::Warning && d.message.contains("'dangling'"))
        );
        assert!(
            graph
                .diagnostics
                .iter()
                .any(|d| d.severity == GraphSeverity::Error && d.message.contains("'bad'"))
        );
    }

    #[test]
    fn triggers_become_nodes_wired_to_entry_steps() {
        let mut s = sop(vec![step(1, "a"), step(2, "b")]);
        s.triggers = vec![
            SopTrigger::Manual,
            SopTrigger::Webhook {
                path: "/hook".into(),
            },
        ];
        let graph = SopGraph::from_sop(&s);

        let trigger_nodes: Vec<&GraphNode> = graph
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Trigger)
            .collect();
        assert_eq!(trigger_nodes.len(), 2);
        assert_eq!(trigger_nodes[0].step, TRIGGER_NODE_BASE);
        assert_eq!(trigger_nodes[1].step, TRIGGER_NODE_BASE + 1);
        assert_eq!(trigger_nodes[1].subtitle.as_deref(), Some("webhook:/hook"));

        // Only step 1 has no inbound flow, so both triggers wire to it alone.
        assert_eq!(
            flow_wires(&graph, FlowRole::Trigger),
            vec![(TRIGGER_NODE_BASE, 1), (TRIGGER_NODE_BASE + 1, 1)]
        );
    }

    fn spec(name: &str, params: serde_json::Value, output: Option<serde_json::Value>) -> ToolSpec {
        ToolSpec {
            name: name.to_string(),
            description: String::new(),
            parameters: std::sync::Arc::new(params),
            output,
            param_domains: Default::default(),
        }
    }

    fn call(tool: &str, args: serde_json::Value) -> super::super::types::PlannedToolCall {
        super::super::types::PlannedToolCall {
            tool: tool.to_string(),
            args,
            pinned: None,
        }
    }

    #[test]
    fn binding_produces_typed_data_wire() {
        let producer = step(1, "a");
        let mut consumer = step(2, "b");
        consumer.calls = vec![call(
            "sink",
            serde_json::json!({"value": "{{steps.1.calls.0}}"}),
        )];
        let mut producer = producer;
        producer.calls = vec![call("src", serde_json::json!({}))];

        let specs = ToolSpecs::from([
            (
                "src".to_string(),
                spec(
                    "src",
                    serde_json::json!({"type": "object"}),
                    Some(serde_json::json!({"type": "object"})),
                ),
            ),
            (
                "sink".to_string(),
                spec(
                    "sink",
                    serde_json::json!({
                        "type": "object",
                        "properties": {"value": {"type": "object"}}
                    }),
                    None,
                ),
            ),
        ]);
        let graph = SopGraph::from_sop_with_specs(&sop(vec![producer, consumer]), &specs);

        let data: Vec<_> = graph
            .wires
            .iter()
            .filter(|w| w.class == PinClass::Data)
            .collect();
        assert_eq!(data.len(), 1);
        assert_eq!((data[0].from_step, data[0].to_step), (1, 2));
        assert_eq!(data[0].from_pin.as_deref(), Some("calls.0"));
        assert_eq!(data[0].to_pin.as_deref(), Some("calls.0.value"));
        assert!(!graph.has_errors());
    }

    #[test]
    fn binding_type_mismatch_is_blocking_error() {
        let mut producer = step(1, "a");
        producer.calls = vec![call("src", serde_json::json!({}))];
        let mut consumer = step(2, "b");
        consumer.calls = vec![call(
            "sink",
            serde_json::json!({"value": "{{steps.1.calls.0}}"}),
        )];

        let specs = ToolSpecs::from([
            (
                "src".to_string(),
                spec(
                    "src",
                    serde_json::json!({"type": "object"}),
                    Some(serde_json::json!({"type": "string"})),
                ),
            ),
            (
                "sink".to_string(),
                spec(
                    "sink",
                    serde_json::json!({
                        "type": "object",
                        "properties": {"value": {"type": "object"}}
                    }),
                    None,
                ),
            ),
        ]);
        let graph = SopGraph::from_sop_with_specs(&sop(vec![producer, consumer]), &specs);

        assert!(graph.wires.iter().all(|w| w.class != PinClass::Data));
        assert!(graph.has_errors());
        assert!(graph.diagnostics.iter().any(|d| {
            d.severity == GraphSeverity::Error && d.message.contains("data type mismatch")
        }));
    }

    #[test]
    fn binding_to_missing_step_is_error() {
        let mut consumer = step(1, "a");
        consumer.calls = vec![call(
            "sink",
            serde_json::json!({"value": "{{steps.9.calls.0}}"}),
        )];
        let specs = ToolSpecs::from([(
            "sink".to_string(),
            spec("sink", serde_json::json!({"type": "object"}), None),
        )]);
        let graph = SopGraph::from_sop_with_specs(&sop(vec![consumer]), &specs);
        assert!(graph.diagnostics.iter().any(|d| {
            d.severity == GraphSeverity::Error && d.message.contains("does not exist")
        }));
    }

    #[test]
    fn layout_assigns_columns_by_longest_path() {
        let mut s3 = step(3, "join");
        s3.routing.depends_on = vec![1, 2];
        let graph = SopGraph::from_sop(&sop(vec![step(1, "a"), step(2, "b"), s3]));
        let col_of = |n: u32| {
            graph
                .layout
                .positions
                .iter()
                .find(|p| p.step == n)
                .unwrap()
                .col
        };
        assert_eq!(col_of(1), 0);
        assert_eq!(col_of(2), 1);
        assert_eq!(col_of(3), 2);
        assert_eq!(graph.layout.columns, 3);
    }

    fn run(status: SopRunStatus, current: u32, results: Vec<SopStepResult>) -> SopRun {
        SopRun {
            run_id: "r1".into(),
            sop_name: "g".into(),
            trigger_event: SopEvent {
                source: SopTriggerSource::Manual,
                topic: None,
                payload: None,
                timestamp: "2026-01-01T00:00:00Z".into(),
            },
            frame_marker_id: String::new(),
            status,
            current_step: current,
            total_steps: 3,
            started_at: "2026-01-01T00:00:00Z".into(),
            completed_at: None,
            step_results: results,
            waiting_since: None,
            llm_calls_saved: 0,
            revision: 0,
            revision_base: 0,
        }
    }

    fn result(step: u32, status: SopStepStatus) -> SopStepResult {
        SopStepResult {
            effective_agent: None,
            step_number: step,
            status,
            output: String::new(),
            started_at: "2026-01-01T00:00:00Z".into(),
            completed_at: None,
            tool_calls: Vec::new(),
        }
    }

    #[test]
    fn run_overlay_projects_step_states_and_skips_trigger_nodes() {
        let mut s = sop(vec![step(1, "a"), step(2, "b"), step(3, "c")]);
        s.triggers = vec![SopTrigger::Manual];
        let graph = SopGraph::from_sop(&s);
        let overlay = RunOverlay::project(
            &graph,
            &run(
                SopRunStatus::Running,
                2,
                vec![result(1, SopStepStatus::Completed)],
            ),
        );

        assert_eq!(overlay.nodes.len(), 3, "trigger nodes carry no run state");
        let state_of = |n: u32| overlay.nodes.iter().find(|o| o.step == n).unwrap().state;
        assert_eq!(state_of(1), NodeRunState::Completed);
        assert_eq!(state_of(2), NodeRunState::Active);
        assert_eq!(state_of(3), NodeRunState::Pending);
        assert!(!overlay.waiting);
        assert!(!overlay.paused);
    }

    #[test]
    fn run_overlay_terminal_run_has_no_active_node() {
        let graph = SopGraph::from_sop(&sop(vec![step(1, "a"), step(2, "b")]));
        let overlay = RunOverlay::project(
            &graph,
            &run(
                SopRunStatus::Failed,
                2,
                vec![result(1, SopStepStatus::Completed)],
            ),
        );
        let state_of = |n: u32| overlay.nodes.iter().find(|o| o.step == n).unwrap().state;
        assert_eq!(state_of(1), NodeRunState::Completed);
        assert_eq!(state_of(2), NodeRunState::Pending);
    }

    #[test]
    fn run_overlay_carries_captured_tool_calls() {
        use super::super::types::StepToolCall;
        let graph = SopGraph::from_sop(&sop(vec![step(1, "a"), step(2, "b")]));
        let mut r1 = result(1, SopStepStatus::Completed);
        r1.tool_calls = vec![StepToolCall {
            index: 0,
            tool: "calculator".into(),
            args: serde_json::json!({"function": "add"}),
            success: true,
            output: "3".into(),
            output_data: Some(serde_json::json!({"value": 3})),
            error: None,
            duration_ms: 5,
        }];
        let overlay = RunOverlay::project(&graph, &run(SopRunStatus::Running, 2, vec![r1]));

        let node = |n: u32| overlay.nodes.iter().find(|o| o.step == n).unwrap();
        assert_eq!(node(1).tool_calls.len(), 1);
        assert_eq!(node(1).tool_calls[0].tool, "calculator");
        assert!(node(2).tool_calls.is_empty(), "unreached step has no calls");
    }

    /// Pins the JSON wire shape consumed by zerocode's `SopGraphView` mirror
    /// (`apps/zerocode/src/client.rs`, mod sop_method_tests). If this changes,
    /// fix both sides together.
    #[test]
    fn graph_serializes_to_the_pinned_wire_shape() {
        let mut s1 = step(1, "First");
        s1.schema = Some(StepSchema {
            input: Some(serde_json::json!({"type": "object"})),
            output: None,
        });
        s1.routing.switch = vec![SwitchRule {
            name: "pr".into(),
            when: None,
            goto: None,
        }];
        let mut s = sop(vec![s1]);
        s.triggers = vec![SopTrigger::Manual];
        let graph = SopGraph::from_sop(&s);

        let value = serde_json::to_value(&graph).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "nodes": [
                    {
                        "step": 1,
                        "title": "First",
                        "kind": "step",
                        "inputs": [
                            {"class": "flow", "name": "in", "required": false},
                            {"class": "data", "name": "input", "data_type": "object", "required": true}
                        ],
                        "outputs": [
                            {"class": "flow", "name": "pr", "required": false}
                        ]
                    },
                    {
                        "step": TRIGGER_NODE_BASE,
                        "title": "manual",
                        "kind": "trigger",
                        "subtitle": "manual",
                        "trigger_index": 0,
                        "inputs": [],
                        "outputs": [
                            {"class": "flow", "name": "event", "required": false}
                        ]
                    }
                ],
                "wires": [
                    {"class": "flow", "from_step": TRIGGER_NODE_BASE, "to_step": 1, "flow_role": "trigger", "from_pin": "event"}
                ],
                "diagnostics": [
                    {"severity": "warning", "step": 1, "message": "switch port 'pr' has no target"}
                ],
                "layout": {
                    "positions": [
                        {"step": 1, "col": 1, "row": 0},
                        {"step": TRIGGER_NODE_BASE, "col": 0, "row": 0}
                    ],
                    "columns": 2,
                    "rows": 1,
                    "geometry": {
                        "node_w": 210.0,
                        "node_h": 84.0,
                        "col_gap": 130.0,
                        "row_gap": 46.0,
                        "origin": 24.0
                    }
                }
            })
        );
    }

    #[test]
    fn render_graph_text_covers_all_formats() {
        let mut s1 = step(1, "First");
        s1.routing.switch = vec![SwitchRule {
            name: "pr".into(),
            when: None,
            goto: Some(2),
        }];
        let graph = SopGraph::from_sop(&sop(vec![s1, step(2, "Second"), step(9, "Ghost")]));

        let outline = render_graph_text(&graph, &TextGraphFormat::Outline);
        assert!(outline.contains("1. First -> 2"));

        let adjacency = render_graph_text(&graph, &TextGraphFormat::Adjacency);
        assert!(adjacency.contains("1 -> 2 [switch:pr]"));

        let json = render_graph_text(&graph, &TextGraphFormat::Json);
        assert!(serde_json::from_str::<SopGraph>(&json).is_ok());
    }
}
