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
#[serde(rename_all = "snake_case")]
pub enum PinClass {
    Flow,
    Data,
}

/// The role a flow wire plays, so surfaces can style edges distinctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowRole {
    /// Normal successor edge (explicit `next` or implicit linear order).
    Sequence,
    /// A `depends_on` precedence edge.
    Dependency,
    /// An `on_failure: goto` edge.
    Failure,
}

/// A typed pin on a node. `data_type` is `None` for flow pins and for data
/// pins whose schema omits a `type` (treated as `Any`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphPin {
    pub class: PinClass,
    pub name: String,
    /// JSON Schema `type` for data pins; `None` means Any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_type: Option<String>,
    /// Required pins must be satisfied by an inbound wire.
    pub required: bool,
}

/// A single node in the projected graph, one per SOP step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNode {
    pub step: u32,
    pub title: String,
    pub inputs: Vec<GraphPin>,
    pub outputs: Vec<GraphPin>,
}

/// An inferred connection. Flow wires carry a `FlowRole`; data wires carry
/// the pin names they connect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[serde(rename_all = "snake_case")]
pub enum GraphSeverity {
    Warning,
    Error,
}

/// A structural diagnostic carried on the projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphDiagnostic {
    pub severity: GraphSeverity,
    pub step: u32,
    pub message: String,
}

/// The full projected graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SopGraph {
    pub nodes: Vec<GraphNode>,
    pub wires: Vec<GraphWire>,
    pub diagnostics: Vec<GraphDiagnostic>,
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
    let mut outputs = vec![GraphPin {
        class: PinClass::Flow,
        name: "out".to_string(),
        data_type: None,
        required: false,
    }];

    if let Some(schema) = &step.schema {
        inputs.extend(data_pins(schema.input.as_ref(), "input"));
        outputs.extend(data_pins(schema.output.as_ref(), "output"));
    }

    GraphNode {
        step: step.number,
        title: step.title.clone(),
        inputs,
        outputs,
    }
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
        let nodes: Vec<GraphNode> = sop.steps.iter().map(node_for).collect();
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
        }

        Self::infer_data_wires(&nodes, &mut wires, &mut diagnostics);

        Self {
            nodes,
            wires,
            diagnostics,
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
}
