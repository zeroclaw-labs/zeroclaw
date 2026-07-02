//! Blueprint graph projection of a `Sop`.
//!

use serde::{Deserialize, Serialize};

use super::types::{Sop, SopStep};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PinClass {
    Flow,
    Data,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FlowRole {
    Sequence,
    Dependency,
    Failure,
    Switch,
    Trigger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Step,
    Trigger,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct GraphPin {
    pub class: PinClass,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
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

pub const TRIGGER_NODE_BASE: u32 = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
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
pub struct GraphDiagnostic {
    pub severity: GraphSeverity,
    pub step: u32,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct NodePosition {
    pub step: u32,
    pub col: u32,
    pub row: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct GraphLayout {
    pub positions: Vec<NodePosition>,
    pub columns: u32,
    pub rows: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SopGraph {
    pub nodes: Vec<GraphNode>,
    pub wires: Vec<GraphWire>,
    pub diagnostics: Vec<GraphDiagnostic>,
    pub layout: GraphLayout,
}

pub enum TextGraphFormat {
    Outline,
    Adjacency,
    Json,
}

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

    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == GraphSeverity::Error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NodeRunState {
    Pending,
    Active,
    Completed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct NodeRunOverlay {
    pub step: u32,
    pub state: NodeRunState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
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
