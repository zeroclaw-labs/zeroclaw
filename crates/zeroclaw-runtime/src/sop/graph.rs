//! Blueprint graph projection of a `Sop`.
//!
//! The graph is a PROJECTION inferred on demand from the existing `Sop`
//! model. It is never persisted: surfaces render whatever this yields.
//! Wires are inferred from routing (flow) and step schemas (data).

use serde::{Deserialize, Serialize};

use super::types::{Sop, SopStep};

/// A pin's data class. Flow pins carry execution order; Data pins carry
/// typed values between step schemas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PinClass {
    Flow,
    Data,
}

/// The role a flow wire plays, so surfaces can style edges distinctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FlowRole {
    /// Normal successor edge (explicit `next` or implicit linear order).
    Sequence,
    /// A `depends_on` precedence edge.
    Dependency,
    /// An `on_failure: goto` edge.
    Failure,
    /// A named switch-port edge. The port label rides in the wire's `from_pin`.
    Switch,
    /// An edge from a trigger node into the SOP's entry step. Fan-in from every
    /// declared trigger source (webhook, mqtt, cron, filesystem, ...).
    Trigger,
}

/// What a projected node represents. Steps are the SOP's own steps; a Trigger
/// node is one declared `SopTrigger`, projected so a surface can render the
/// event fan-in the way n8n renders a webhook feeding downstream branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Step,
    Trigger,
}

/// A typed pin on a node. `data_type` is `None` for flow pins and for data
/// pins whose schema omits a `type` (treated as `Any`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct GraphPin {
    pub class: PinClass,
    pub name: String,
    /// JSON Schema `type` for data pins; `None` means Any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    /// Required pins must be satisfied by an inbound wire.
    pub required: bool,
}

/// A single node in the projected graph. One per SOP step, plus one per
/// declared trigger. `kind` distinguishes the two so surfaces style them
/// distinctly; trigger nodes carry the trigger's display string in `subtitle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct GraphNode {
    pub step: u32,
    pub title: String,
    #[serde(default = "node_kind_step")]
    pub kind: NodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// For `Trigger` nodes, the index of this trigger in `sop.triggers`, so a
    /// surface can bind a canvas click straight to the matching trigger editor
    /// without recomputing the synthetic id offset. `None` for step nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_index: Option<u32>,
    pub inputs: Vec<GraphPin>,
    pub outputs: Vec<GraphPin>,
}

fn node_kind_step() -> NodeKind {
    NodeKind::Step
}

/// Trigger nodes are numbered from this base so their synthetic ids never
/// collide with real 1-based step numbers.
pub const TRIGGER_NODE_BASE: u32 = 1_000_000;

/// An inferred connection. Flow wires carry a `FlowRole`; data wires carry
/// the pin names they connect.
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

/// Severity of a graph diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum GraphSeverity {
    Warning,
    Error,
}

/// A structural diagnostic carried on the projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct GraphDiagnostic {
    pub severity: GraphSeverity,
    pub step: u32,
    pub message: String,
}

/// A node's canonical grid slot in the layered layout. `col` is the layer
/// index (0 = a root with no predecessors), `row` packs siblings within a
/// column. Surfaces map these onto pixels or terminal cells; the slot itself
/// is single-sourced here so no surface reinvents graph shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct NodePosition {
    pub step: u32,
    pub col: u32,
    pub row: u32,
}

/// The layered layout of a projected graph. `columns`/`rows` are the grid
/// extents so a surface can size its viewport without re-deriving them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct GraphLayout {
    pub positions: Vec<NodePosition>,
    pub columns: u32,
    pub rows: u32,
}

/// The full projected graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SopGraph {
    pub nodes: Vec<GraphNode>,
    pub wires: Vec<GraphWire>,
    pub diagnostics: Vec<GraphDiagnostic>,
    /// Layered x/y placement walked from the flow edges. Single source for
    /// every surface's node-graph editor; never persisted.
    pub layout: GraphLayout,
}

/// Surfaces render the projection through this trait. The backend owns the
/// projection; each surface walks `SopGraph` and renders whatever it gets.
pub trait GraphRender {
    type Output;
    fn render(&self, graph: &SopGraph) -> Self::Output;
}

/// Text renderers over the projection. Surfaces that want plain text reuse
/// these instead of hand-walking the graph.
pub enum TextGraphFormat {
    /// One line per node with its outbound flow edges.
    Outline,
    /// `from -> to [role]` adjacency, one edge per line.
    Adjacency,
    /// Pretty-printed JSON of the whole projection.
    Json,
}

/// Render the projection to text in the requested format. Diagnostics, when
/// present, are appended under a `diagnostics:` section (Json embeds them in
/// the serialized graph instead).
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

/// Extract the JSON Schema `type` string from a schema fragment. A bare
/// string fragment (the parser's fallback) is treated as that type name.
fn schema_type(fragment: &serde_json::Value) -> Option<String> {
    match fragment {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(map) => {
            map.get("type").and_then(|t| t.as_str()).map(str::to_string)
        }
        _ => None,
    }
}

/// Whether `required` is set on a schema object (defaults to true for data
/// inputs so missing producers are surfaced; an explicit `required: false`
/// opts out).
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

/// Project one declared trigger into a source node. The node has a single flow
/// output (`event`) and no inputs; its `subtitle` is the trigger's canonical
/// display string. Its synthetic id is `TRIGGER_NODE_BASE + index` so it never
/// collides with a real step number.
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

/// Human labels for a trigger node: a short kind title and the full display
/// string. Derived from the `SopTrigger` variant; no surface hardcodes these.
fn trigger_labels(trigger: &super::types::SopTrigger) -> (String, String) {
    use super::types::SopTrigger;
    let kind = match trigger {
        SopTrigger::Mqtt { .. } => "mqtt".to_string(),
        SopTrigger::Webhook { .. } => "webhook".to_string(),
        SopTrigger::Cron { .. } => "cron".to_string(),
        SopTrigger::Peripheral { .. } => "peripheral".to_string(),
        SopTrigger::Filesystem { .. } => "filesystem".to_string(),
        SopTrigger::Calendar { .. } => "calendar".to_string(),
        SopTrigger::Channel { channel, .. } => channel.clone(),
        SopTrigger::Manual => "manual".to_string(),
    };
    (kind, trigger.to_string())
}

/// Strict pin type check: identical, or Any on either side. No widening.
fn types_compatible(from: Option<&str>, to: Option<&str>) -> bool {
    match (from, to) {
        (None, _) | (_, None) => true,
        (Some(a), Some(b)) => a == b,
    }
}

impl SopGraph {
    /// Project a `Sop` into its graph form. Pure: no I/O, no persistence.
    pub fn from_sop(sop: &Sop) -> Self {
        let mut nodes: Vec<GraphNode> = sop.steps.iter().map(node_for).collect();
        let valid_steps: std::collections::HashSet<u32> =
            sop.steps.iter().map(|s| s.number).collect();

        let mut wires = Vec::new();
        let mut diagnostics = Vec::new();

        for (idx, step) in sop.steps.iter().enumerate() {
            // ── Flow: sequence ──
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
                    if let Some(following) = sop.steps.get(idx + 1) {
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

            // ── Flow: dependencies ──
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

            // ── Flow: failure goto ──
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

            // ── Flow: switch ports ──
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

        // ── Trigger fan-in ──
        // Every declared trigger becomes a source node wired into the SOP's
        // entry step(s). An entry step is one with no inbound step-to-step flow
        // (Sequence/Dependency/Switch/Failure). This mirrors how a webhook feeds
        // downstream branches in n8n; it is pure projection of `sop.triggers`.
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

    /// Layered layout walked from the projected flow edges. A node's column is
    /// `1 + max(col of every flow predecessor)`; roots (no inbound flow) land
    /// in column 0. Rows pack per column in step order. Cycles are broken by
    /// treating an already-visited node as column 0 during resolution, so a
    /// failure-goto back-edge never loops forever. This is the single source of
    /// node placement: every surface reads it and never re-derives shape.
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

    /// Infer data wires: an upstream node's `output` pin feeds a downstream
    /// node's `input` pin when the producer precedes the consumer and the
    /// types are compatible. Required inputs with no producer are flagged.
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

    /// Whether the projection carries any `Error`-severity diagnostic.
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == GraphSeverity::Error)
    }
}

// ── Run overlay projection (slice 8) ─────────────────────────────

/// Per-node execution state, projected from a `SopRun` onto a `SopGraph`.
/// An immutable snapshot for watching a run progress, like a Blueprint
/// executing. Inferred on demand; never persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NodeRunState {
    /// Not yet reached by the run.
    Pending,
    /// The step the run is currently on (running, waiting, or paused).
    Active,
    Completed,
    Failed,
    Skipped,
}

/// Run state for one graph node, keyed by the node's step number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct NodeRunOverlay {
    pub step: u32,
    pub state: NodeRunState,
}

/// The full run overlay: the run-level status plus per-node states. Surfaces
/// align each entry to its `SopGraph` node by `step` and highlight it. The
/// `waiting` / `paused` flags let a surface show why an Active node is held.
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
    /// Project a run onto a graph. Each node's state is derived from the run's
    /// recorded step results first (terminal states win), then the run's
    /// current position (the live step is Active while the run is non-terminal),
    /// then Pending for anything not yet reached. Step results are authoritative
    /// because a step can be Skipped without advancing `current_step` linearly.
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
    use crate::sop::step_contract::{StepFailure, StepRouting};
    use crate::sop::types::{SopExecutionMode, SopPriority, SopStep, StepSchema};

    fn step(number: u32, title: &str) -> SopStep {
        SopStep {
            number,
            title: title.to_string(),
            ..SopStep::default()
        }
    }

    fn sop_with(steps: Vec<SopStep>) -> Sop {
        Sop {
            name: "g".to_string(),
            description: "d".to_string(),
            version: "1.0.0".to_string(),
            priority: SopPriority::Normal,
            execution_mode: SopExecutionMode::Supervised,
            triggers: Vec::new(),
            steps,
            cooldown_secs: 0,
            max_concurrent: 1,
            location: None,
            deterministic: false,
        }
    }

    fn typed(number: u32, input: Option<&str>, output: Option<&str>) -> SopStep {
        let to_frag = |t: &str| serde_json::json!({"type": t});
        SopStep {
            number,
            title: format!("s{number}"),
            schema: Some(StepSchema {
                input: input.map(to_frag),
                output: output.map(to_frag),
            }),
            ..SopStep::default()
        }
    }

    #[test]
    fn one_node_per_step_with_flow_pins() {
        let graph = SopGraph::from_sop(&sop_with(vec![step(1, "a"), step(2, "b")]));
        assert_eq!(graph.nodes.len(), 2);
        for node in &graph.nodes {
            assert!(node.inputs.iter().any(|p| p.class == PinClass::Flow));
            assert!(node.outputs.iter().any(|p| p.class == PinClass::Flow));
        }
    }

    #[test]
    fn implicit_linear_flow_when_no_explicit_next() {
        let graph = SopGraph::from_sop(&sop_with(vec![step(1, "a"), step(2, "b"), step(3, "c")]));
        let seq: Vec<(u32, u32)> = graph
            .wires
            .iter()
            .filter(|w| w.flow_role == Some(FlowRole::Sequence))
            .map(|w| (w.from_step, w.to_step))
            .collect();
        assert_eq!(seq, vec![(1, 2), (2, 3)]);
    }

    #[test]
    fn explicit_next_overrides_linear() {
        let mut s1 = step(1, "a");
        s1.routing = StepRouting {
            next: Some(3),
            ..StepRouting::default()
        };
        let graph = SopGraph::from_sop(&sop_with(vec![s1, step(2, "b"), step(3, "c")]));
        assert!(
            graph
                .wires
                .iter()
                .any(|w| w.flow_role == Some(FlowRole::Sequence)
                    && w.from_step == 1
                    && w.to_step == 3)
        );
    }

    #[test]
    fn switch_ports_project_named_edges_and_output_pins() {
        use super::super::step_contract::SwitchRule;
        let mut s1 = step(1, "router");
        s1.routing = StepRouting {
            switch: vec![
                SwitchRule {
                    name: "pull_request".into(),
                    when: Some("$.event == \"pr\"".into()),
                    goto: Some(2),
                },
                SwitchRule {
                    name: "catch-all".into(),
                    when: None,
                    goto: Some(3),
                },
            ],
            ..StepRouting::default()
        };
        let graph = SopGraph::from_sop(&sop_with(vec![s1, step(2, "b"), step(3, "c")]));
        let pr = graph.wires.iter().find(|w| {
            w.flow_role == Some(FlowRole::Switch) && w.from_pin.as_deref() == Some("pull_request")
        });
        assert!(pr.is_some(), "pull_request switch edge projected");
        assert_eq!(pr.unwrap().to_step, 2);
        let catch = graph.wires.iter().find(|w| {
            w.flow_role == Some(FlowRole::Switch) && w.from_pin.as_deref() == Some("catch-all")
        });
        assert_eq!(catch.unwrap().to_step, 3);
        let router = graph.nodes.iter().find(|n| n.step == 1).unwrap();
        let ports: Vec<&str> = router.outputs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(ports, vec!["pull_request", "catch-all"]);
    }

    #[test]
    fn switch_port_dangling_target_is_error() {
        use super::super::step_contract::SwitchRule;
        let mut s1 = step(1, "router");
        s1.routing = StepRouting {
            switch: vec![SwitchRule {
                name: "gone".into(),
                when: None,
                goto: Some(99),
            }],
            ..StepRouting::default()
        };
        let graph = SopGraph::from_sop(&sop_with(vec![s1]));
        assert!(
            graph
                .diagnostics
                .iter()
                .any(|d| d.severity == GraphSeverity::Error && d.message.contains("gone"))
        );
    }

    #[test]
    fn dependency_edge_projected() {
        let mut s2 = step(2, "b");
        s2.routing = StepRouting {
            depends_on: vec![1],
            ..StepRouting::default()
        };
        let graph = SopGraph::from_sop(&sop_with(vec![step(1, "a"), s2]));
        assert!(
            graph
                .wires
                .iter()
                .any(|w| w.flow_role == Some(FlowRole::Dependency)
                    && w.from_step == 1
                    && w.to_step == 2)
        );
    }

    #[test]
    fn failure_goto_edge_projected() {
        let mut s1 = step(1, "a");
        s1.on_failure = StepFailure::Goto { step: 2 };
        let graph = SopGraph::from_sop(&sop_with(vec![s1, step(2, "b")]));
        assert!(
            graph
                .wires
                .iter()
                .any(|w| w.flow_role == Some(FlowRole::Failure)
                    && w.from_step == 1
                    && w.to_step == 2)
        );
    }

    #[test]
    fn dangling_flow_refs_are_errors() {
        let mut s1 = step(1, "a");
        s1.routing = StepRouting {
            next: Some(99),
            depends_on: vec![88],
            ..StepRouting::default()
        };
        s1.on_failure = StepFailure::Goto { step: 77 };
        let graph = SopGraph::from_sop(&sop_with(vec![s1]));
        let msgs: Vec<&str> = graph
            .diagnostics
            .iter()
            .map(|d| d.message.as_str())
            .collect();
        assert!(msgs.iter().any(|m| m.contains("next target step 99")));
        assert!(msgs.iter().any(|m| m.contains("depends_on target step 88")));
        assert!(msgs.iter().any(|m| m.contains("goto target step 77")));
        assert!(graph.has_errors());
    }

    #[test]
    fn identical_data_types_wire_up() {
        let graph = SopGraph::from_sop(&sop_with(vec![
            typed(1, None, Some("string")),
            typed(2, Some("string"), None),
        ]));
        assert!(
            graph
                .wires
                .iter()
                .any(|w| w.class == PinClass::Data && w.from_step == 1 && w.to_step == 2)
        );
        assert!(!graph.has_errors());
    }

    #[test]
    fn any_type_accepts_either_side() {
        // Concrete output → Any input (input schema present but no `type`).
        let mut consumer = SopStep {
            number: 2,
            title: "b".to_string(),
            ..SopStep::default()
        };
        consumer.schema = Some(StepSchema {
            input: Some(serde_json::json!({})),
            output: None,
        });
        let graph = SopGraph::from_sop(&sop_with(vec![typed(1, None, Some("number")), consumer]));
        assert!(
            graph
                .wires
                .iter()
                .any(|w| w.class == PinClass::Data && w.from_step == 1 && w.to_step == 2),
            "concrete output must feed an Any input"
        );
        assert!(!graph.has_errors());

        // Any output → concrete input.
        let mut producer = SopStep {
            number: 1,
            title: "a".to_string(),
            ..SopStep::default()
        };
        producer.schema = Some(StepSchema {
            input: None,
            output: Some(serde_json::json!({})),
        });
        let graph2 = SopGraph::from_sop(&sop_with(vec![producer, typed(2, Some("string"), None)]));
        assert!(
            graph2
                .wires
                .iter()
                .any(|w| w.class == PinClass::Data && w.from_step == 1 && w.to_step == 2),
            "Any output must feed a concrete input"
        );
        assert!(!graph2.has_errors());
    }

    #[test]
    fn type_mismatch_is_warning_and_required_unsatisfied_is_error() {
        let graph = SopGraph::from_sop(&sop_with(vec![
            typed(1, None, Some("number")),
            typed(2, Some("string"), None),
        ]));
        assert!(
            graph
                .diagnostics
                .iter()
                .any(|d| d.severity == GraphSeverity::Warning && d.message.contains("mismatch"))
        );
        assert!(graph
            .diagnostics
            .iter()
            .any(|d| d.severity == GraphSeverity::Error && d.message.contains("required input")));
    }

    #[test]
    fn optional_input_unsatisfied_is_silent() {
        let mut s2 = SopStep {
            number: 2,
            title: "b".to_string(),
            ..SopStep::default()
        };
        s2.schema = Some(StepSchema {
            input: Some(serde_json::json!({"type": "string", "required": false})),
            output: None,
        });
        let graph = SopGraph::from_sop(&sop_with(vec![step(1, "a"), s2]));
        assert!(!graph.has_errors(), "optional unsatisfied must not error");
    }

    #[test]
    fn projection_carries_no_persistence_path() {
        // The graph type is render-only: it has no save method and no
        // field pointing at a file. This compiles only because SopGraph is
        // pure data; the assertion documents intent.
        let graph = SopGraph::from_sop(&sop_with(vec![step(1, "a")]));
        let json = serde_json::to_string(&graph).unwrap();
        assert!(json.contains("\"nodes\""));
    }

    #[test]
    fn text_outline_lists_nodes_and_flow_edges() {
        let graph = SopGraph::from_sop(&sop_with(vec![step(1, "a"), step(2, "b")]));
        let out = render_graph_text(&graph, &TextGraphFormat::Outline);
        assert!(out.contains("1. a -> 2"));
        assert!(out.contains("2. b"));
    }

    #[test]
    fn text_adjacency_one_edge_per_line() {
        let graph = SopGraph::from_sop(&sop_with(vec![step(1, "a"), step(2, "b")]));
        let out = render_graph_text(&graph, &TextGraphFormat::Adjacency);
        assert!(out.contains("1 -> 2 [sequence]"));
    }

    #[test]
    fn text_json_is_parseable_projection() {
        let graph = SopGraph::from_sop(&sop_with(vec![step(1, "a")]));
        let out = render_graph_text(&graph, &TextGraphFormat::Json);
        let back: SopGraph = serde_json::from_str(&out).unwrap();
        assert_eq!(back.nodes.len(), 1);
    }

    fn pos_of(graph: &SopGraph, step: u32) -> (u32, u32) {
        let p = graph
            .layout
            .positions
            .iter()
            .find(|p| p.step == step)
            .expect("position present");
        (p.col, p.row)
    }

    #[test]
    fn layout_lays_linear_chain_across_columns() {
        let graph = SopGraph::from_sop(&sop_with(vec![step(1, "a"), step(2, "b"), step(3, "c")]));
        assert_eq!(pos_of(&graph, 1), (0, 0));
        assert_eq!(pos_of(&graph, 2), (1, 0));
        assert_eq!(pos_of(&graph, 3), (2, 0));
        assert_eq!(graph.layout.columns, 3);
        assert_eq!(graph.layout.rows, 1);
    }

    #[test]
    fn layout_branches_pack_rows_within_a_column() {
        use super::super::step_contract::SwitchRule;
        // Router with an explicit `next: None` on the branch targets so no
        // implicit linear 2->3 edge forms; both branches then share column 1.
        let mut s1 = step(1, "router");
        s1.routing = StepRouting {
            switch: vec![
                SwitchRule {
                    name: "a".into(),
                    when: None,
                    goto: Some(2),
                },
                SwitchRule {
                    name: "b".into(),
                    when: None,
                    goto: Some(3),
                },
            ],
            ..StepRouting::default()
        };
        // Break the implicit linear chain by giving step 2 an explicit terminal
        // next (itself is filtered as self-loop upstream; here we simply route
        // it nowhere by pointing next at a nonexistent-free path via depends).
        let mut s2 = step(2, "x");
        s2.routing = StepRouting {
            next: Some(2),
            ..StepRouting::default()
        };
        let graph = SopGraph::from_sop(&sop_with(vec![s1, s2, step(3, "y")]));
        assert_eq!(pos_of(&graph, 1), (0, 0));
        assert_eq!(pos_of(&graph, 2).0, 1);
        assert_eq!(pos_of(&graph, 3).0, 1);
        assert_ne!(pos_of(&graph, 2).1, pos_of(&graph, 3).1);
        assert_eq!(graph.layout.columns, 2);
        assert_eq!(graph.layout.rows, 2);
    }

    #[test]
    fn layout_dependency_fan_in_pushes_successor_right() {
        let mut s3 = step(3, "join");
        s3.routing = StepRouting {
            depends_on: vec![1, 2],
            ..StepRouting::default()
        };
        // steps 1 and 2 have no inbound flow -> column 0; 3 depends on both.
        let mut s1 = step(1, "a");
        s1.routing = StepRouting {
            next: Some(3),
            ..StepRouting::default()
        };
        let mut s2 = step(2, "b");
        s2.routing = StepRouting {
            next: Some(3),
            ..StepRouting::default()
        };
        let graph = SopGraph::from_sop(&sop_with(vec![s1, s2, s3]));
        assert_eq!(pos_of(&graph, 1).0, 0);
        assert_eq!(pos_of(&graph, 2).0, 0);
        assert_eq!(pos_of(&graph, 3).0, 1);
    }

    #[test]
    fn layout_survives_failure_back_edge_cycle() {
        let mut s2 = step(2, "b");
        s2.on_failure = StepFailure::Goto { step: 1 };
        let graph = SopGraph::from_sop(&sop_with(vec![step(1, "a"), s2]));
        // A back-edge 2 -> 1 must not loop the resolver; both get finite cols.
        assert_eq!(graph.layout.positions.len(), 2);
        assert!(graph.layout.columns >= 1);
    }

    #[test]
    fn triggers_project_as_source_nodes_feeding_entry_step() {
        use super::super::types::SopTrigger;
        let mut sop = sop_with(vec![step(1, "a"), step(2, "b")]);
        sop.triggers = vec![
            SopTrigger::Webhook {
                path: "/hook".into(),
            },
            SopTrigger::Cron {
                expression: "0 9 * * *".into(),
            },
        ];
        let graph = SopGraph::from_sop(&sop);
        let triggers: Vec<&GraphNode> = graph
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Trigger)
            .collect();
        assert_eq!(triggers.len(), 2);
        assert!(triggers.iter().any(|n| n.title == "webhook"));
        assert!(triggers.iter().any(|n| n.title == "cron"));
        // Each trigger fans into the entry step (step 1, no inbound flow).
        for tnode in &triggers {
            assert!(graph.wires.iter().any(|w| {
                w.flow_role == Some(FlowRole::Trigger)
                    && w.from_step == tnode.step
                    && w.to_step == 1
            }));
        }
        // Trigger nodes have no data inputs and one flow output.
        for tnode in &triggers {
            assert!(tnode.inputs.is_empty());
            assert_eq!(tnode.outputs.len(), 1);
        }
    }

    #[test]
    fn channel_trigger_projects_as_node_titled_by_channel_kind() {
        use super::super::types::SopTrigger;
        let mut sop = sop_with(vec![step(1, "a"), step(2, "b")]);
        sop.triggers = vec![SopTrigger::Channel {
            channel: "telegram".into(),
            alias: Some("ops".into()),
            condition: None,
        }];
        let graph = SopGraph::from_sop(&sop);
        let node = graph
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Trigger)
            .expect("channel trigger node");
        assert_eq!(node.title, "telegram");
        assert_eq!(node.subtitle.as_deref(), Some("channel:telegram/ops"));
        assert!(graph.wires.iter().any(|w| {
            w.flow_role == Some(FlowRole::Trigger) && w.from_step == node.step && w.to_step == 1
        }));
    }

    #[test]
    fn trigger_node_sits_left_of_entry_step_in_layout() {
        use super::super::types::SopTrigger;
        let mut sop = sop_with(vec![step(1, "a"), step(2, "b")]);
        sop.triggers = vec![SopTrigger::Manual];
        let graph = SopGraph::from_sop(&sop);
        let trigger = graph
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Trigger)
            .expect("trigger node");
        let tcol = graph
            .layout
            .positions
            .iter()
            .find(|p| p.step == trigger.step)
            .expect("trigger position")
            .col;
        assert_eq!(tcol, 0);
        assert_eq!(pos_of(&graph, 1).0, 1);
    }

    #[test]
    fn text_renderers_append_diagnostics() {
        let mut s1 = step(1, "a");
        s1.routing = StepRouting {
            next: Some(99),
            ..StepRouting::default()
        };
        let graph = SopGraph::from_sop(&sop_with(vec![s1]));
        let out = render_graph_text(&graph, &TextGraphFormat::Outline);
        assert!(out.contains("diagnostics:"));
        assert!(out.contains("[error]"));
    }

    // ── Run overlay (slice 8) ────────────────────────────────────

    fn step_result(
        number: u32,
        status: super::super::types::SopStepStatus,
    ) -> super::super::types::SopStepResult {
        super::super::types::SopStepResult {
            step_number: number,
            status,
            output: String::new(),
            started_at: "t".into(),
            completed_at: None,
        }
    }

    fn run_with(
        status: super::super::types::SopRunStatus,
        current_step: u32,
        total_steps: u32,
        results: Vec<super::super::types::SopStepResult>,
    ) -> super::super::types::SopRun {
        super::super::types::SopRun {
            run_id: "run-1".into(),
            sop_name: "g".into(),
            trigger_event: super::super::types::SopEvent {
                source: super::super::types::SopTriggerSource::Manual,
                topic: None,
                payload: None,
                timestamp: "t".into(),
            },
            frame_marker_id: "m".into(),
            status,
            current_step,
            total_steps,
            started_at: "t".into(),
            completed_at: None,
            step_results: results,
            waiting_since: None,
            llm_calls_saved: 0,
        }
    }

    fn state_for(overlay: &RunOverlay, step: u32) -> NodeRunState {
        overlay
            .nodes
            .iter()
            .find(|n| n.step == step)
            .expect("node present")
            .state
    }

    #[test]
    fn overlay_marks_current_step_active_mid_run() {
        use super::super::types::{SopRunStatus, SopStepStatus};
        let graph = SopGraph::from_sop(&sop_with(vec![step(1, "a"), step(2, "b"), step(3, "c")]));
        let run = run_with(
            SopRunStatus::Running,
            2,
            3,
            vec![step_result(1, SopStepStatus::Completed)],
        );
        let overlay = RunOverlay::project(&graph, &run);
        assert_eq!(state_for(&overlay, 1), NodeRunState::Completed);
        assert_eq!(state_for(&overlay, 2), NodeRunState::Active);
        assert_eq!(state_for(&overlay, 3), NodeRunState::Pending);
        assert!(!overlay.waiting);
        assert!(!overlay.paused);
    }

    #[test]
    fn overlay_projects_step_result_states() {
        use super::super::types::{SopRunStatus, SopStepStatus};
        let graph = SopGraph::from_sop(&sop_with(vec![step(1, "a"), step(2, "b"), step(3, "c")]));
        let run = run_with(
            SopRunStatus::Running,
            3,
            3,
            vec![
                step_result(1, SopStepStatus::Completed),
                step_result(2, SopStepStatus::Skipped),
            ],
        );
        let overlay = RunOverlay::project(&graph, &run);
        assert_eq!(state_for(&overlay, 1), NodeRunState::Completed);
        assert_eq!(state_for(&overlay, 2), NodeRunState::Skipped);
        assert_eq!(state_for(&overlay, 3), NodeRunState::Active);
    }

    #[test]
    fn overlay_terminal_run_has_no_active_node() {
        use super::super::types::{SopRunStatus, SopStepStatus};
        let graph = SopGraph::from_sop(&sop_with(vec![step(1, "a"), step(2, "b")]));
        let run = run_with(
            SopRunStatus::Failed,
            2,
            2,
            vec![
                step_result(1, SopStepStatus::Completed),
                step_result(2, SopStepStatus::Failed),
            ],
        );
        let overlay = RunOverlay::project(&graph, &run);
        assert_eq!(state_for(&overlay, 1), NodeRunState::Completed);
        assert_eq!(state_for(&overlay, 2), NodeRunState::Failed);
        assert!(
            overlay
                .nodes
                .iter()
                .all(|n| n.state != NodeRunState::Active)
        );
    }

    #[test]
    fn overlay_surfaces_waiting_and_paused() {
        use super::super::types::SopRunStatus;
        let graph = SopGraph::from_sop(&sop_with(vec![step(1, "a"), step(2, "b")]));

        let waiting = RunOverlay::project(
            &graph,
            &run_with(SopRunStatus::WaitingApproval, 1, 2, vec![]),
        );
        assert!(waiting.waiting);
        assert_eq!(state_for(&waiting, 1), NodeRunState::Active);

        let paused = RunOverlay::project(
            &graph,
            &run_with(SopRunStatus::PausedCheckpoint, 2, 2, vec![]),
        );
        assert!(paused.paused);
        assert_eq!(state_for(&paused, 2), NodeRunState::Active);
    }

    #[test]
    fn overlay_round_trips_through_serde() {
        use super::super::types::SopRunStatus;
        let graph = SopGraph::from_sop(&sop_with(vec![step(1, "a"), step(2, "b")]));
        let overlay = RunOverlay::project(&graph, &run_with(SopRunStatus::Running, 1, 2, vec![]));
        let json = serde_json::to_string(&overlay).unwrap();
        let back: RunOverlay = serde_json::from_str(&json).unwrap();
        assert_eq!(back, overlay);
    }
}
