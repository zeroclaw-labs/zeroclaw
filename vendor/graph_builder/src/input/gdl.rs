use crate::graph::csr::{CsrLayout, DirectedCsrGraph, NodeValues, UndirectedCsrGraph};
use crate::index::Idx;
use crate::input::EdgeList;

use gdl::CypherValue;

#[cfg(feature = "dotgraph")]
use crate::input::DotGraph;
#[cfg(feature = "dotgraph")]
use linereader::LineReader;
#[cfg(feature = "dotgraph")]
use std::fmt::Write;
#[cfg(feature = "dotgraph")]
use std::hash::Hash;

/// A wrapper around [`gdl::CypherValue`] to allow custom From implementations.
pub struct MyCypherValue<'a>(&'a CypherValue);

impl From<MyCypherValue<'_>> for () {
    fn from(_: MyCypherValue) -> Self {}
}

macro_rules! impl_from_cypher_value {
    ($enum:path, $ty:ty) => {
        impl<'a> ::std::convert::From<$crate::input::gdl::MyCypherValue<'a>> for $ty {
            fn from(cv: $crate::input::gdl::MyCypherValue) -> Self {
                if let $enum(f) = cv.0 {
                    *f as $ty
                } else {
                    panic!("expected {} value", stringify!($ty))
                }
            }
        }
    };
}

impl_from_cypher_value!(CypherValue::Float, f32);
impl_from_cypher_value!(CypherValue::Float, f64);
impl_from_cypher_value!(CypherValue::Integer, i32);
impl_from_cypher_value!(CypherValue::Integer, i64);
impl_from_cypher_value!(CypherValue::Integer, isize);
impl_from_cypher_value!(CypherValue::Integer, u32);
impl_from_cypher_value!(CypherValue::Integer, u64);
impl_from_cypher_value!(CypherValue::Integer, usize);

impl<'gdl, NI, EV> From<&'gdl gdl::Graph> for EdgeList<NI, EV>
where
    NI: Idx,
    EV: From<MyCypherValue<'gdl>> + Default + Send + Sync,
{
    fn from(gdl_graph: &'gdl gdl::Graph) -> Self {
        let edges = gdl_graph
            .relationships()
            .map(|r| {
                let source = gdl_graph.get_node(r.source()).unwrap().id();
                let target = gdl_graph.get_node(r.target()).unwrap().id();

                let value = if let Some(k) = r.property_keys().next() {
                    EV::from(MyCypherValue(r.property_value(k).unwrap()))
                } else {
                    EV::default()
                };

                (NI::new(source), NI::new(target), value)
            })
            .collect::<Vec<_>>();

        EdgeList::new(edges)
    }
}

impl<'gdl, NV> From<&'gdl gdl::Graph> for NodeValues<NV>
where
    NV: From<MyCypherValue<'gdl>> + Default + Send + Sync,
{
    fn from(gdl_graph: &'gdl gdl::Graph) -> Self {
        let mut node_values = Vec::with_capacity(gdl_graph.node_count());
        node_values.resize_with(gdl_graph.node_count(), || NV::default());

        gdl_graph.nodes().for_each(|n| {
            if let Some(k) = n.property_keys().next() {
                node_values[n.id()] = NV::from(MyCypherValue(n.property_value(k).unwrap()));
            }
        });

        NodeValues::new(node_values)
    }
}

#[cfg(feature = "dotgraph")]
impl<NI, Label> From<&gdl::Graph> for DotGraph<NI, Label>
where
    NI: Idx,
    Label: Idx + Hash,
{
    /// Converts the given GDL graph into a .graph input string.
    ///
    /// Node labels need to be numeric, however GDL does not support numeric
    /// labels. In order to circumvent this, node labels need to be prefixed
    /// with a single character, e.g. `(n:L0)` to declare label `0`.
    fn from(gdl_graph: &gdl::Graph) -> Self {
        fn degree(gdl_graph: &gdl::Graph, node: &gdl::graph::Node) -> usize {
            let mut degree = 0;

            for rel in gdl_graph.relationships() {
                if rel.source() == node.variable() {
                    degree += 1;
                }
                if rel.target() == node.variable() {
                    degree += 1;
                }
            }
            degree
        }

        let header = format!(
            "t {} {}",
            gdl_graph.node_count(),
            gdl_graph.relationship_count()
        );

        let mut nodes_string = String::from("");

        let mut sorted_nodes = gdl_graph.nodes().collect::<Vec<_>>();
        sorted_nodes.sort_by_key(|node| node.id());

        for node in sorted_nodes {
            let id = node.id();
            let label = node.labels().next().expect("Single label expected");
            let degree = degree(gdl_graph, node);
            let _ = writeln!(nodes_string, "v {id} {} {degree}", &label[1..]);
        }

        let mut rels_string = String::from("");

        let mut sorted_rels = gdl_graph.relationships().collect::<Vec<_>>();
        sorted_rels.sort_by_key(|rel| (rel.source(), rel.target()));

        for rel in sorted_rels {
            let source_id = gdl_graph
                .get_node(rel.source())
                .expect("Source expected")
                .id();
            let target_id = gdl_graph
                .get_node(rel.target())
                .expect("Target expected")
                .id();
            let _ = writeln!(rels_string, "e {source_id} {target_id}");
        }

        let input = format!("{header}\n{nodes_string}{rels_string}");
        let reader = LineReader::new(input.as_bytes());

        DotGraph::<NI, Label>::try_from(reader).expect("GDL to .graph conversion failed")
    }
}

impl<'a, NI, NV, EV> From<(&'a gdl::Graph, CsrLayout)> for DirectedCsrGraph<NI, NV, EV>
where
    NI: Idx,
    NV: From<MyCypherValue<'a>> + Default + Copy + Send + Sync,
    EV: From<MyCypherValue<'a>> + Default + Copy + Send + Sync,
{
    fn from((gdl_graph, csr_layout): (&'a gdl::Graph, CsrLayout)) -> Self {
        let node_values = NodeValues::from(gdl_graph);
        let edge_list = EdgeList::from(gdl_graph);
        DirectedCsrGraph::from((node_values, edge_list, csr_layout))
    }
}

impl<NI, NV, EV> From<(gdl::Graph, CsrLayout)> for DirectedCsrGraph<NI, NV, EV>
where
    NI: Idx,
    for<'a> NV: From<MyCypherValue<'a>> + Default + Copy + Send + Sync,
    for<'a> EV: From<MyCypherValue<'a>> + Default + Copy + Send + Sync,
{
    fn from((gdl_graph, csr_layout): (gdl::Graph, CsrLayout)) -> Self {
        let node_values = NodeValues::from(&gdl_graph);
        let edge_list = EdgeList::from(&gdl_graph);
        DirectedCsrGraph::from((node_values, edge_list, csr_layout))
    }
}

impl<'a, NI, NV, EV> From<(&'a gdl::Graph, CsrLayout)> for UndirectedCsrGraph<NI, NV, EV>
where
    NI: Idx,
    NV: From<MyCypherValue<'a>> + Default + Copy + Send + Sync,
    EV: From<MyCypherValue<'a>> + Default + Copy + Send + Sync,
{
    fn from((gdl_graph, csr_layout): (&'a gdl::Graph, CsrLayout)) -> Self {
        let node_values = NodeValues::from(gdl_graph);
        let edge_list = EdgeList::from(gdl_graph);
        UndirectedCsrGraph::from((node_values, edge_list, csr_layout))
    }
}

impl<NI, NV, EV> From<(gdl::Graph, CsrLayout)> for UndirectedCsrGraph<NI, NV, EV>
where
    NI: Idx,
    for<'a> NV: From<MyCypherValue<'a>> + Default + Copy + Send + Sync,
    for<'a> EV: From<MyCypherValue<'a>> + Default + Copy + Send + Sync,
{
    fn from((gdl_graph, csr_layout): (gdl::Graph, CsrLayout)) -> Self {
        let node_values = NodeValues::from(&gdl_graph);
        let edge_list = EdgeList::from(&gdl_graph);
        UndirectedCsrGraph::from((node_values, edge_list, csr_layout))
    }
}
