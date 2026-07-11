//! Shared serde projection types for the SOP Blueprint graph. Single source
//! of the graph wire shape: the runtime builds these, the gateway exports
//! their JSON Schema, and the zerocode TUI deserializes them off RPC.

use serde::{Deserialize, Serialize};

/// Node-id offset for synthetic trigger nodes, keeping them disjoint from
/// real step numbers. Trigger `i` gets node id `TRIGGER_NODE_BASE + i`.
pub const TRIGGER_NODE_BASE: u32 = 1_000_000;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum_macros::IntoStaticStr,
)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PinClass {
    /// Execution-order edge: which step runs after which.
    Flow,
    /// Typed data edge derived from a `{{steps.N}}` binding.
    Data,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum_macros::IntoStaticStr,
)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// Why a flow wire exists. Mirrors the `StepRouting`/`StepFailure` field it
/// was derived from, so an editor can write edits back to the right place.
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
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

impl FlowRole {
    pub fn describe(&self) -> &'static str {
        match self {
            FlowRole::Sequence => "Implicit fallthrough or explicit routing.next.",
            FlowRole::Dependency => {
                "routing.depends_on fan-in: source must complete before target runs."
            }
            FlowRole::Failure => "on_failure: goto recovery edge.",
            FlowRole::Switch => "Named conditional port from routing.switch.",
            FlowRole::Trigger => "Derived from the SOP's triggers; read-only, never hand-wired.",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            FlowRole::Sequence => "next step",
            FlowRole::Dependency => "waits for",
            FlowRole::Failure => "on failure",
            FlowRole::Switch => "branch",
            FlowRole::Trigger => "trigger",
        }
    }
}

impl PinClass {
    pub fn describe(&self) -> &'static str {
        match self {
            PinClass::Flow => "Execution-order edge: which step runs after which.",
            PinClass::Data => "Typed data edge derived from a {{steps.N}} binding.",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            PinClass::Flow => "flow",
            PinClass::Data => "data",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// An executable SOP step.
    #[default]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    #[serde(default)]
    pub kind: NodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_index: Option<u32>,
    pub inputs: Vec<GraphPin>,
    pub outputs: Vec<GraphPin>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// A directed edge between two nodes. `flow_role` is set for flow wires;
/// data wires carry the producer/consumer pin names instead.
pub struct GraphWire {
    pub class: PinClass,
    pub from_step: u32,
    pub to_step: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_role: Option<FlowRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_pin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_pin: Option<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum_macros::IntoStaticStr,
)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// Grid placement for one node: column = longest flow path from an entry,
/// row = order of insertion within that column. `x`/`y` carry a persisted
/// canvas coordinate when the node has been dragged; absent otherwise.
pub struct NodePosition {
    pub step: u32,
    pub col: u32,
    pub row: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
}

/// Canonical canvas geometry: the pixel box and inter-slot pitch every
/// surface renders a node with, plus the seed origin a grid slot maps to.
/// This is the single registry both the web canvas and the zerocode TUI read
/// from (the web off `GraphLayout.geometry`, zerocode off [`LayoutGeometry::CANONICAL`])
/// so neither surface hardcodes its own drifting copy. A slot at
/// `(col, row)` seeds to `(origin + col*(node_w+col_gap), origin + row*(node_h+row_gap))`,
/// and a persisted `NodePosition.x`/`y` lives in that same pixel space.
pub const LAYOUT_NODE_W: f64 = 210.0;
pub const LAYOUT_NODE_H: f64 = 84.0;
pub const LAYOUT_COL_GAP: f64 = 130.0;
pub const LAYOUT_ROW_GAP: f64 = 46.0;
pub const LAYOUT_ORIGIN: f64 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// Canvas geometry carried on every serialized graph so the web canvas reads
/// placement pitch from the wire instead of a local literal. The values are
/// fixed by [`LayoutGeometry::CANONICAL`]; the struct rides on `GraphLayout`
/// only to expose them to non-Rust surfaces.
pub struct LayoutGeometry {
    pub node_w: f64,
    pub node_h: f64,
    pub col_gap: f64,
    pub row_gap: f64,
    pub origin: f64,
}

impl LayoutGeometry {
    pub const CANONICAL: Self = Self {
        node_w: LAYOUT_NODE_W,
        node_h: LAYOUT_NODE_H,
        col_gap: LAYOUT_COL_GAP,
        row_gap: LAYOUT_ROW_GAP,
        origin: LAYOUT_ORIGIN,
    };

    /// Column pitch: the pixel distance between two adjacent grid columns.
    pub const fn col_pitch(&self) -> f64 {
        self.node_w + self.col_gap
    }

    /// Row pitch: the pixel distance between two adjacent grid rows.
    pub const fn row_pitch(&self) -> f64 {
        self.node_h + self.row_gap
    }
}

impl Default for LayoutGeometry {
    fn default() -> Self {
        Self::CANONICAL
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// Deterministic auto-layout so every surface renders the same picture
/// without a client-side layout engine.
pub struct GraphLayout {
    #[serde(default)]
    pub positions: Vec<NodePosition>,
    #[serde(default)]
    pub columns: u32,
    #[serde(default)]
    pub rows: u32,
    #[serde(default)]
    pub geometry: LayoutGeometry,
}

impl Default for GraphLayout {
    fn default() -> Self {
        Self {
            positions: Vec::new(),
            columns: 0,
            rows: 0,
            geometry: LayoutGeometry::CANONICAL,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// The full projected graph: nodes, wires, validation diagnostics, and a
/// precomputed layout. Serialized as-is over RPC (`sops/graph`) and HTTP.
pub struct SopGraph {
    #[serde(default)]
    pub nodes: Vec<GraphNode>,
    #[serde(default)]
    pub wires: Vec<GraphWire>,
    #[serde(default)]
    pub diagnostics: Vec<GraphDiagnostic>,
    #[serde(default)]
    pub layout: GraphLayout,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// Per-node execution state projected from a run's step results.
#[serde(rename_all = "snake_case")]
pub enum NodeRunState {
    /// Not reached yet (or run ended before reaching it).
    #[default]
    Pending,
    /// The run's current step while the run is live.
    Active,
    Completed,
    Failed,
    Skipped,
}

impl NodeRunState {
    pub fn describe(&self) -> &'static str {
        match self {
            NodeRunState::Pending => "Not reached yet (or the run ended before reaching it).",
            NodeRunState::Active => "The run's current step while the run is live.",
            NodeRunState::Completed => "The step finished successfully.",
            NodeRunState::Failed => "The step errored and did not complete.",
            NodeRunState::Skipped => "The step was routed around and never ran.",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            NodeRunState::Pending => "pending",
            NodeRunState::Active => "running",
            NodeRunState::Completed => "done",
            NodeRunState::Failed => "failed",
            NodeRunState::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// One legend row: a graph concept plus its human description. The stable
/// `key` is the snake_case wire value the canvas maps tones/handles against.
pub struct LegendEntry {
    pub key: String,
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
/// The canonical legend for the SOP canvas: flow-wire roles, pin classes, and
/// run states. The single authority a surface reads to render a legend and
/// per-handle/per-wire hover context, so no surface hardcodes these lists.
pub struct GraphLegend {
    pub flow_roles: Vec<LegendEntry>,
    pub pin_classes: Vec<LegendEntry>,
    pub run_states: Vec<LegendEntry>,
}

impl GraphLegend {
    pub fn canonical() -> Self {
        let flow_roles = [
            FlowRole::Sequence,
            FlowRole::Dependency,
            FlowRole::Failure,
            FlowRole::Switch,
            FlowRole::Trigger,
        ]
        .into_iter()
        .map(|role| LegendEntry {
            key: <&'static str>::from(role).to_string(),
            label: role.label().to_string(),
            description: role.describe().to_string(),
        })
        .collect();
        let pin_classes = [PinClass::Flow, PinClass::Data]
            .into_iter()
            .map(|class| LegendEntry {
                key: <&'static str>::from(class).to_string(),
                label: class.label().to_string(),
                description: class.describe().to_string(),
            })
            .collect();
        let run_states = [
            NodeRunState::Pending,
            NodeRunState::Active,
            NodeRunState::Completed,
            NodeRunState::Failed,
            NodeRunState::Skipped,
        ]
        .into_iter()
        .map(|state| LegendEntry {
            key: run_state_key(state).to_string(),
            label: state.label().to_string(),
            description: state.describe().to_string(),
        })
        .collect();
        Self {
            flow_roles,
            pin_classes,
            run_states,
        }
    }
}

fn run_state_key(state: NodeRunState) -> &'static str {
    match state {
        NodeRunState::Pending => "pending",
        NodeRunState::Active => "active",
        NodeRunState::Completed => "completed",
        NodeRunState::Failed => "failed",
        NodeRunState::Skipped => "skipped",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_and_wires_round_trip_through_the_snake_case_wire_shape() {
        let node = GraphNode {
            step: 1,
            title: "publish".into(),
            kind: NodeKind::Step,
            subtitle: None,
            trigger_index: None,
            inputs: vec![GraphPin {
                class: PinClass::Data,
                name: "calls.0.body".into(),
                data_type: Some("string".into()),
                required: true,
            }],
            outputs: vec![GraphPin {
                class: PinClass::Flow,
                name: "out".into(),
                data_type: None,
                required: false,
            }],
        };
        let wire = GraphWire {
            class: PinClass::Data,
            from_step: 1,
            to_step: 2,
            flow_role: None,
            from_pin: Some("result".into()),
            to_pin: Some("calls.0.body".into()),
        };
        let graph = SopGraph {
            nodes: vec![node],
            wires: vec![wire],
            diagnostics: vec![GraphDiagnostic {
                severity: GraphSeverity::Error,
                step: 2,
                message: "missing".into(),
            }],
            layout: GraphLayout::default(),
        };

        let json = serde_json::to_value(&graph).unwrap();
        assert_eq!(json["nodes"][0]["inputs"][0]["class"], "data");
        assert_eq!(json["nodes"][0]["outputs"][0]["class"], "flow");
        assert!(json["nodes"][0]["outputs"][0].get("data_type").is_none());
        assert_eq!(json["wires"][0]["class"], "data");
        assert_eq!(json["diagnostics"][0]["severity"], "error");

        let back: SopGraph = serde_json::from_value(json).unwrap();
        assert_eq!(back, graph);
    }

    #[test]
    fn flow_role_serializes_all_variants_snake_case() {
        for (role, wire) in [
            (FlowRole::Sequence, "sequence"),
            (FlowRole::Dependency, "dependency"),
            (FlowRole::Failure, "failure"),
            (FlowRole::Switch, "switch"),
            (FlowRole::Trigger, "trigger"),
        ] {
            assert_eq!(serde_json::to_value(role).unwrap(), wire);
        }
    }

    #[test]
    fn canonical_legend_covers_every_variant_with_matching_keys() {
        let legend = GraphLegend::canonical();
        assert_eq!(legend.flow_roles.len(), 5);
        assert_eq!(legend.pin_classes.len(), 2);
        assert_eq!(legend.run_states.len(), 5);
        for entry in legend
            .flow_roles
            .iter()
            .chain(&legend.pin_classes)
            .chain(&legend.run_states)
        {
            assert!(!entry.key.is_empty());
            assert!(!entry.description.is_empty());
        }
        let flow_keys: Vec<&str> = legend.flow_roles.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(
            flow_keys,
            ["sequence", "dependency", "failure", "switch", "trigger"]
        );
        let state_keys: Vec<&str> = legend.run_states.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(
            state_keys,
            ["pending", "active", "completed", "failed", "skipped"]
        );
    }
}
