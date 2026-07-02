//! Blueprint graph projection of a `Sop`.
//!
//! Projects the linear step list plus routing metadata into a node/wire
//! graph for visual editors (web node canvas, zerocode SOP pane) and text
//! renderers. Pure projection: building a graph never mutates the SOP.

use serde::{Deserialize, Serialize};

use super::types::{Sop, SopStep};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PinClass {
    /// Execution-order edge: which step runs after which.
    Flow,
    /// Typed data edge inferred from step input/output schemas.
    Data,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// Why a flow wire exists. Mirrors the `StepRouting`/`StepFailure` field it
/// was derived from, so an editor can write edits back to the right place.
#[serde(rename_all = "snake_case")]
pub enum FlowRole {
    /// Implicit fallthrough or explicit `routing.next`.
    Sequence,
    /// `routing.depends_on` fan-in: source must complete before target runs.
    Dependency,
    /// `on_failure: goto` recovery edge.
    Failure,
    /// Named conditional port from `routing.switch`.
    Switch,
    /// Derived from the SOP's triggers; read-only, never hand-wired.
    Trigger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// An executable SOP step.
    Step,
    /// A synthetic entry node representing one of the SOP's triggers.
    Trigger,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// One connection point on a node. Flow pins order execution; data pins
/// carry the step's declared input/output schema type.
pub struct GraphPin {
    pub class: PinClass,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// A node in the projected graph: one SOP step, or one synthetic trigger
/// entry (`step >= TRIGGER_NODE_BASE`, `trigger_index` set).
pub struct GraphNode {
    pub step: u32,
    pub title: String,
    #[serde(default = "node_kind_step")]
    pub kind: NodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_index: Option<u32>,
    pub inputs: Vec<GraphPin>,
    pub outputs: Vec<GraphPin>,
}

fn node_kind_step() -> NodeKind {
    NodeKind::Step
}

/// Node-id offset for synthetic trigger nodes, keeping them disjoint from
/// real step numbers. Trigger `i` gets node id `TRIGGER_NODE_BASE + i`.
pub const TRIGGER_NODE_BASE: u32 = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// A directed edge between two nodes. `flow_role` is set for flow wires;
/// data wires carry the producer/consumer pin names instead.
pub struct GraphWire {
    pub class: PinClass,
    pub from_step: u32,
    pub to_step: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow_role: Option<FlowRole>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_pin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_pin: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum GraphSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// A validation finding anchored to a step. Errors block saving
/// (`validate_sop_strict`); warnings render but do not block.
pub struct GraphDiagnostic {
    pub severity: GraphSeverity,
    pub step: u32,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// Grid placement for one node: column = longest flow path from an entry,
/// row = order of insertion within that column.
pub struct NodePosition {
    pub step: u32,
    pub col: u32,
    pub row: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// Deterministic auto-layout so every surface renders the same picture
/// without a client-side layout engine.
pub struct GraphLayout {
    pub positions: Vec<NodePosition>,
    pub columns: u32,
    pub rows: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// The full projected graph: nodes, wires, validation diagnostics, and a
/// precomputed layout. Serialized as-is over RPC (`sops/graph`) and HTTP.
pub struct SopGraph {
    pub nodes: Vec<GraphNode>,
    pub wires: Vec<GraphWire>,
    pub diagnostics: Vec<GraphDiagnostic>,
    pub layout: GraphLayout,
}

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
                let label = match (wire.class, wire.flow_role) {
                    (PinClass::Flow, Some(FlowRole::Switch)) => match &wire.from_pin {
                        Some(port) => format!("switch:{port}"),
                        None => "switch".to_string(),
                    },
                    (PinClass::Flow, Some(role)) => format!("{role:?}").to_lowercase(),
                    (PinClass::Data, _) => "data".to_string(),
                    (PinClass::Flow, None) => "flow".to_string(),
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
        let sev = match diag.severity {
            GraphSeverity::Error => "error",
            GraphSeverity::Warning => "warning",
        };
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

fn data_pins(schema_fragment: Option<&serde_json::Value>, name: &str) -> Vec<GraphPin> {
    match schema_fragment {
        Some(fragment) => vec![GraphPin {
            class: PinClass::Data,
            name: name.to_string(),
            data_type: schema_type(fragment),
            required: schema_required(fragment),
        }],
        None => Vec::new(),
    }
}

fn node_for(step: &SopStep) -> GraphNode {
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

    if let Some(schema) = &step.schema {
        inputs.extend(data_pins(schema.input.as_ref(), "input"));
        outputs.extend(data_pins(schema.output.as_ref(), "output"));
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

impl SopGraph {
    /// Project a SOP into a graph. Never fails: unresolvable references
    /// (missing steps, dangling switch ports, unsatisfied required inputs)
    /// become diagnostics instead of errors, so editors can render and fix
    /// broken drafts.
    pub fn from_sop(sop: &Sop) -> Self {
        let mut nodes: Vec<GraphNode> = sop.steps.iter().map(node_for).collect();
        let valid_steps: std::collections::HashSet<u32> =
            sop.steps.iter().map(|s| s.number).collect();

        let mut wires = Vec::new();
        let mut diagnostics = Vec::new();

        for (idx, step) in sop.steps.iter().enumerate() {
            match step.routing.next {
                Some(next) => {
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
                None => {
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

        Self::infer_data_wires(&nodes, &mut wires, &mut diagnostics);

        let layout = Self::layout(&nodes, &wires);

        Self {
            nodes,
            wires,
            diagnostics,
            layout,
        }
    }

    fn layout(nodes: &[GraphNode], wires: &[GraphWire]) -> GraphLayout {
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
            });
            columns = columns.max(c + 1);
            rows = rows.max(*r + 1);
            *r += 1;
        }

        GraphLayout {
            positions,
            columns,
            rows,
        }
    }

    fn infer_data_wires(
        nodes: &[GraphNode],
        wires: &mut Vec<GraphWire>,
        diagnostics: &mut Vec<GraphDiagnostic>,
    ) {
        for (consumer_idx, consumer) in nodes.iter().enumerate() {
            for input in consumer.inputs.iter().filter(|p| p.class == PinClass::Data) {
                let mut satisfied = false;
                for producer in nodes[..consumer_idx].iter() {
                    for output in producer
                        .outputs
                        .iter()
                        .filter(|p| p.class == PinClass::Data)
                    {
                        if types_compatible(output.data_type.as_deref(), input.data_type.as_deref())
                        {
                            wires.push(GraphWire {
                                class: PinClass::Data,
                                from_step: producer.step,
                                to_step: consumer.step,
                                flow_role: None,
                                from_pin: Some(output.name.clone()),
                                to_pin: Some(input.name.clone()),
                            });
                            satisfied = true;
                        } else {
                            diagnostics.push(GraphDiagnostic {
                                severity: GraphSeverity::Warning,
                                step: consumer.step,
                                message: format!(
                                    "data type mismatch: step {} output `{}` ({}) does not satisfy input `{}` ({})",
                                    producer.step,
                                    output.name,
                                    output.data_type.as_deref().unwrap_or("any"),
                                    input.name,
                                    input.data_type.as_deref().unwrap_or("any"),
                                ),
                            });
                        }
                    }
                }
                if input.required && !satisfied {
                    diagnostics.push(GraphDiagnostic {
                        severity: GraphSeverity::Error,
                        step: consumer.step,
                        message: format!(
                            "required input `{}` has no upstream producer of a compatible type",
                            input.name
                        ),
                    });
                }
            }
        }
    }

    /// True when any diagnostic is `Error` severity; such a graph fails
    /// `validate_sop_strict` and cannot be saved.
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == GraphSeverity::Error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// Per-node execution state projected from a run's step results.
#[serde(rename_all = "snake_case")]
pub enum NodeRunState {
    /// Not reached yet (or run ended before reaching it).
    Pending,
    /// The run's current step while the run is live.
    Active,
    Completed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// Run state for one step node. Trigger nodes carry no run state and are
/// omitted from overlays.
pub struct NodeRunOverlay {
    pub step: u32,
    pub state: NodeRunState,
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
                let recorded = run
                    .step_results
                    .iter()
                    .find(|r| r.step_number == node.step)
                    .map(|r| match r.status {
                        SopStepStatus::Completed => NodeRunState::Completed,
                        SopStepStatus::Failed => NodeRunState::Failed,
                        SopStepStatus::Skipped => NodeRunState::Skipped,
                    });
                let state = match recorded {
                    Some(s) => s,
                    None if !terminal_run && node.step == run.current_step => NodeRunState::Active,
                    None => NodeRunState::Pending,
                };
                NodeRunOverlay {
                    step: node.step,
                    state,
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

    #[test]
    fn data_pins_wire_by_type_and_unsatisfied_required_input_is_error() {
        let mut producer = step(1, "a");
        producer.schema = Some(StepSchema {
            input: None,
            output: Some(serde_json::json!({"type": "object"})),
        });
        let mut ok_consumer = step(2, "b");
        ok_consumer.schema = Some(StepSchema {
            input: Some(serde_json::json!({"type": "object"})),
            output: None,
        });
        let mut orphan = step(3, "c");
        orphan.schema = Some(StepSchema {
            input: Some(serde_json::json!({"type": "string", "required": true})),
            output: None,
        });
        let graph = SopGraph::from_sop(&sop(vec![producer, ok_consumer, orphan]));

        let data: Vec<_> = graph
            .wires
            .iter()
            .filter(|w| w.class == PinClass::Data)
            .collect();
        assert_eq!(data.len(), 1);
        assert_eq!((data[0].from_step, data[0].to_step), (1, 2));

        assert!(graph.diagnostics.iter().any(|d| {
            d.severity == GraphSeverity::Error
                && d.step == 3
                && d.message.contains("no upstream producer")
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
        }
    }

    fn result(step: u32, status: SopStepStatus) -> SopStepResult {
        SopStepResult {
            step_number: step,
            status,
            output: String::new(),
            started_at: "2026-01-01T00:00:00Z".into(),
            completed_at: None,
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
                    {"severity": "warning", "step": 1, "message": "switch port 'pr' has no target"},
                    {"severity": "error", "step": 1, "message": "required input `input` has no upstream producer of a compatible type"}
                ],
                "layout": {
                    "positions": [
                        {"step": 1, "col": 1, "row": 0},
                        {"step": TRIGGER_NODE_BASE, "col": 0, "row": 0}
                    ],
                    "columns": 2,
                    "rows": 1
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
