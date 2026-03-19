use std::{
    convert::TryFrom,
    fs::File,
    hash::Hash,
    io::Read,
    marker::PhantomData,
    mem::ManuallyDrop,
    ops::Range,
    path::Path,
    sync::atomic::{AtomicUsize, Ordering::Acquire},
};

use atomic::Atomic;
use dashmap::DashMap;
use fxhash::FxHashMap;
use linereader::LineReader;
use rayon::prelude::*;

use crate::{
    graph::csr::{sort_targets, Csr},
    graph::Target,
    index::Idx,
    Error, Graph, NodeValues, SharedMut, UndirectedDegrees, UndirectedNeighbors,
};

use super::{edgelist::EdgeList, InputCapabilities, InputPath};

/// DotGraph (the name is based on the file ending `.graph`) is a textual
/// description of a node labeled graph primarily used as input for subgraph
/// isomorphism libraries. It has been introduced
/// [here](https://github.com/RapidsAtHKUST/SubgraphMatching#input) and is also
/// supported by the
/// [subgraph-matching](https://crates.io/crates/subgraph-matching) crate.
///
/// A graph starts with 't N M' where N is the number of nodes and M is the
/// number of edges. A node and an edge are formatted as 'v nodeId labelId
/// degree' and 'e nodeId nodeId' respectively. Note that the format requires
/// that the node id starts at 0 and the range is `0..N`.
///
/// # Example
///
/// The following graph contains 5 nodes and 6 relationships. The first line
/// contains that meta information. The following 5 lines contain one node
/// description per line, e.g., `v 0 0 2` translates to node `0` with label `0`
/// and a degree of `2`. Following the nodes, the remaining lines describe
/// edges, e.g., `e 0 1` represents an edge connecting nodes `0` and `1`.
///
/// ```ignore
/// > cat my_graph.graph
/// t 5 6
/// v 0 0 2
/// v 1 1 3
/// v 2 2 3
/// v 3 1 2
/// v 4 2 2
/// e 0 1
/// e 0 2
/// e 1 2
/// e 1 3
/// e 2 4
/// e 3 4
/// ```
pub struct DotGraphInput<NI, Label>
where
    NI: Idx,
    Label: Idx,
{
    _phantom: PhantomData<(NI, Label)>,
}

impl<NI, Label> Default for DotGraphInput<NI, Label>
where
    NI: Idx,
    Label: Idx,
{
    fn default() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl<NI: Idx, Label: Idx> InputCapabilities<NI> for DotGraphInput<NI, Label> {
    type GraphInput = DotGraph<NI, Label>;
}

pub struct DotGraph<NI, Label>
where
    NI: Idx,
    Label: Idx,
{
    pub labels: Vec<Label>,
    pub edge_list: EdgeList<NI, ()>,
    pub max_degree: NI,
    pub max_label: Label,
    pub label_frequency: FxHashMap<Label, usize>,
}

impl<NI, Label> DotGraph<NI, Label>
where
    NI: Idx,
    Label: Idx + Hash,
{
    pub fn node_count(&self) -> NI {
        NI::new(self.labels.len())
    }

    pub fn label_count(&self) -> usize {
        self.max_label.index() + 1
    }

    pub fn max_label_frequency(&self) -> usize {
        self.label_frequency
            .values()
            .max()
            .cloned()
            .unwrap_or_default()
    }
}

impl<NI, Label, P> TryFrom<InputPath<P>> for DotGraph<NI, Label>
where
    P: AsRef<Path>,
    NI: Idx,
    Label: Idx + Hash,
{
    type Error = Error;

    fn try_from(path: InputPath<P>) -> Result<Self, Self::Error> {
        let file = File::open(path.0.as_ref())?;
        let reader = LineReader::new(file);
        let dot_graph = DotGraph::try_from(reader)?;
        Ok(dot_graph)
    }
}

impl<NI, Label, R> TryFrom<LineReader<R>> for DotGraph<NI, Label>
where
    NI: Idx,
    Label: Idx + Hash,
    R: Read,
{
    type Error = Error;

    /// Converts the given .graph input into a [`DotGraph`].
    fn try_from(mut lines: LineReader<R>) -> Result<Self, Self::Error> {
        let mut header = lines.next_line().expect("missing header line")?;

        // skip "t" char and white space
        header = &header[2..];
        let (node_count, used) = NI::parse(header);
        header = &header[used + 1..];
        let (edge_count, _) = NI::parse(header);

        let mut labels = Vec::<Label>::with_capacity(node_count.index());
        let mut edges = Vec::with_capacity(edge_count.index());

        let mut max_degree = NI::zero();
        let mut max_label = Label::zero();
        let mut label_frequency = FxHashMap::<Label, usize>::default();

        let mut batch = lines.next_batch().expect("missing data")?;

        for _ in 0..node_count.index() {
            if batch.is_empty() {
                batch = lines.next_batch().expect("missing data")?;
            }

            // skip "v" char and white space
            batch = &batch[2..];
            // skip node id since input is always sorted by node id
            let (_, used) = NI::parse(batch);
            batch = &batch[used + 1..];
            let (label, used) = Label::parse(batch);
            batch = &batch[used + 1..];
            let (degree, used) = NI::parse(batch);
            batch = &batch[used + 1..];

            labels.push(label);

            if degree > max_degree {
                max_degree = degree;
            }

            let frequency = label_frequency.entry(label).or_insert_with(|| {
                if label > max_label {
                    max_label = label;
                }
                0
            });
            *frequency += 1;
        }

        for _ in 0..edge_count.index() {
            if batch.is_empty() {
                batch = lines.next_batch().expect("missing data")?;
            }
            // skip "e" char and white space
            batch = &batch[2..];
            let (source, used) = NI::parse(batch);
            batch = &batch[used + 1..];
            let (target, used) = NI::parse(batch);
            batch = &batch[used + 1..];

            edges.push((source, target, ()));
        }

        let edge_list = EdgeList::new(edges);

        Ok(Self {
            labels,
            edge_list,
            max_degree,
            max_label,
            label_frequency,
        })
    }
}

pub struct LabelStats<NI, Label> {
    pub max_degree: NI,
    pub label_count: usize,
    pub max_label: Label,
    pub max_label_frequency: usize,
    pub label_frequency: FxHashMap<Label, usize>,
}

impl<NI, Label> LabelStats<NI, Label>
where
    NI: Idx,
    Label: Idx + Hash,
{
    pub fn from_graph<G>(graph: &G) -> Self
    where
        G: Graph<NI>
            + UndirectedNeighbors<NI>
            + UndirectedDegrees<NI>
            + NodeValues<NI, Label>
            + Send
            + Sync,
    {
        graph.into()
    }
}

impl<NI, Label, G> From<&G> for LabelStats<NI, Label>
where
    NI: Idx,
    Label: Idx + Hash,
    G: Graph<NI>
        + UndirectedNeighbors<NI>
        + UndirectedDegrees<NI>
        + NodeValues<NI, Label>
        + Send
        + Sync,
{
    fn from(graph: &G) -> Self {
        let label_frequency: DashMap<Label, usize> = DashMap::new();
        let max_degree = AtomicUsize::new(usize::MIN);
        let max_label = AtomicUsize::new(usize::MIN);

        rayon::iter::split(0..graph.node_count().index(), |range| {
            if range.len() <= 1 {
                return (range, None);
            }
            let pivot = range.start + (range.end - range.start) / 2;
            (range.start..pivot, Some(pivot..range.end))
        })
        .into_par_iter()
        .for_each(|range: Range<usize>| {
            let mut local_max_degree = NI::new(usize::MIN);
            let mut local_max_label = Label::new(usize::MIN);

            range.into_iter().for_each(|node| {
                let node = NI::new(node);
                let label = graph.node_value(node);

                let mut frequency = label_frequency.entry(*label).or_insert_with(|| {
                    if *label > local_max_label {
                        local_max_label = *label;
                    }
                    0_usize
                });
                *frequency += 1;

                local_max_degree = NI::max(local_max_degree, graph.degree(node));
            });

            update_max_value(local_max_label.index(), &max_label);
            update_max_value(local_max_degree.index(), &max_degree);
        });

        let max_degree = NI::new(max_degree.load(atomic::Ordering::Acquire));
        let max_label = Label::new(max_label.load(atomic::Ordering::Acquire));
        let max_label_frequency = label_frequency
            .iter()
            .map(|ref_multi| *ref_multi.value())
            .max()
            .unwrap_or_default();
        let label_count = label_frequency.len();
        let label_frequency = label_frequency
            .into_iter()
            .collect::<FxHashMap<Label, usize>>();

        Self {
            max_degree,
            label_count,
            max_label,
            max_label_frequency,
            label_frequency,
        }
    }
}

fn update_max_value(new_value: usize, shared: &AtomicUsize) {
    let mut curr_max = shared.load(atomic::Ordering::Relaxed);

    // Even if `curr_max` is outdated, we know that the actual
    // value is higher, since this is the only case where we
    // update the value. We can safely return in that case.
    if new_value < curr_max {
        return;
    }

    while curr_max < new_value {
        if let Err(new_max) = shared.compare_exchange(
            curr_max,
            new_value,
            atomic::Ordering::AcqRel,
            atomic::Ordering::Relaxed,
        ) {
            // CAX failed, we need to try again with the new value.
            curr_max = new_max;
        }
    }
}

pub struct NeighborLabelFrequency<'a, Label> {
    map: &'a FxHashMap<Label, usize>,
}

impl<'a, Label> NeighborLabelFrequency<'a, Label>
where
    Label: Hash + Eq,
{
    fn new(map: &'a FxHashMap<Label, usize>) -> Self {
        Self { map }
    }

    pub fn get(&self, label: Label) -> Option<usize> {
        self.map.get(&label).copied()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter(&self) -> std::collections::hash_map::Iter<Label, usize> {
        self.map.iter()
    }
}

pub struct NeighborLabelFrequencies<Label, NI> {
    pub frequencies: Vec<FxHashMap<Label, usize>>,
    _node_type: PhantomData<NI>,
}

impl<Label, NI> NeighborLabelFrequencies<Label, NI>
where
    NI: Idx,
    Label: Idx + Hash,
{
    pub fn from_graph<G>(graph: &G) -> Self
    where
        G: Graph<NI>
            + UndirectedNeighbors<NI>
            + UndirectedDegrees<NI>
            + NodeValues<NI, Label>
            + Send
            + Sync,
    {
        graph.into()
    }

    pub fn neighbor_frequency(&self, node: NI) -> NeighborLabelFrequency<'_, Label> {
        NeighborLabelFrequency::new(&self.frequencies[node.index()])
    }
}

impl<Label, G, NI> From<&G> for NeighborLabelFrequencies<Label, NI>
where
    NI: Idx,
    Label: Idx + Hash,
    G: Graph<NI>
        + UndirectedNeighbors<NI>
        + UndirectedDegrees<NI>
        + NodeValues<NI, Label>
        + Send
        + Sync,
{
    fn from(graph: &G) -> Self {
        let mut frequencies = Vec::with_capacity(graph.node_count().index());

        (0..graph.node_count().index())
            .into_par_iter()
            .map(|node| {
                let mut frequency = FxHashMap::<Label, usize>::default();

                for &target in graph.neighbors(NI::new(node)) {
                    let target_label = graph.node_value(target);
                    let count = frequency.entry(*target_label).or_insert(0);
                    *count += 1;
                }

                frequency
            })
            .collect_into_vec(&mut frequencies);

        Self {
            frequencies,
            _node_type: PhantomData,
        }
    }
}

pub struct NodeLabelIndex<Label, NI>(Csr<Label, NI, ()>)
where
    NI: Idx,
    Label: Idx;

impl<Label, NI> NodeLabelIndex<Label, NI>
where
    NI: Idx,
    Label: Idx,
{
    pub fn from_stats<F>(node_count: NI, label_stats: &LabelStats<NI, Label>, label_func: F) -> Self
    where
        Label: Hash,
        F: Fn(NI) -> Label + Send + Sync,
    {
        (node_count, label_stats, label_func).into()
    }

    pub fn nodes(&self, label: Label) -> &[NI] {
        self.0.targets(label)
    }
}

impl<Label, NI, F> From<(NI, &LabelStats<NI, Label>, F)> for NodeLabelIndex<Label, NI>
where
    NI: Idx,
    Label: Idx + Hash,
    F: Fn(NI) -> Label + Send + Sync,
{
    fn from((node_count, label_stats, label_func): (NI, &LabelStats<NI, Label>, F)) -> Self {
        let LabelStats {
            label_count,
            max_label,
            label_frequency,
            ..
        } = label_stats;
        // Prefix sum: We insert the offset entries one index to the right and
        // increment the offset of the next label during insert. That way we'll
        // end up with the correct offsets after inserting into `nodes` in the
        // next loop.
        let mut offsets = Vec::with_capacity(label_count.index() + 1);
        offsets.push(Label::zero());

        let mut total = Label::zero();
        for label in Label::zero().range_inclusive(*max_label) {
            offsets.push(total);
            total += Label::new(*label_frequency.get(&label).unwrap_or(&0));
        }

        let offsets = {
            let mut offsets = ManuallyDrop::new(offsets);
            let (ptr, len, cap) = (offsets.as_mut_ptr(), offsets.len(), offsets.capacity());

            // SAFETY: Label and Label::Atomic have the same memory layout
            unsafe {
                let ptr = ptr as *mut Atomic<Label>;
                Vec::from_raw_parts(ptr, len, cap)
            }
        };

        let mut nodes = Vec::<Target<NI, ()>>::with_capacity(node_count.index());
        let nodes_ptr = SharedMut::new(nodes.as_mut_ptr());

        (0..node_count.index()).into_par_iter().for_each(|node| {
            let label = label_func(NI::new(node));
            let next_label = label + Label::new(1);
            let offset = Label::get_and_increment(&offsets[next_label.index()], Acquire);
            // SAFETY: There is exactly one thread that writes at `offset.index()`.
            unsafe {
                nodes_ptr
                    .add(offset.index())
                    .write(Target::new(NI::new(node), ()));
            }
        });

        // SAFETY: The `nodes` vec has `node_count` length and we performed an
        // insert operation for each index (node). Each inserts happens at a
        // unique index which is computed from the `offset` array.
        unsafe {
            nodes.set_len(node_count.index());
        }

        let offsets = {
            let mut offsets = ManuallyDrop::new(offsets);
            let (ptr, len, cap) = (offsets.as_mut_ptr(), offsets.len(), offsets.capacity());

            // SAFETY: Label and Label::Atomic have the same memory layout
            unsafe {
                let ptr = ptr as *mut _;
                Vec::from_raw_parts(ptr, len, cap)
            }
        };

        sort_targets(&offsets, &mut nodes);

        let offsets = offsets.into_boxed_slice();
        let nodes = nodes.into_boxed_slice();

        let csr = Csr::new(offsets, nodes);

        Self(csr)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::input::edgelist::Edges;
    use crate::input::InputPath;
    use crate::{CsrLayout, UndirectedCsrGraph};

    use super::*;

    const TEST_GRAPH: [&str; 3] = [env!("CARGO_MANIFEST_DIR"), "resources", "test.graph"];

    #[test]
    fn dotgraph_from_file() {
        let path = TEST_GRAPH.iter().collect::<PathBuf>();
        let graph = DotGraph::<usize, usize>::try_from(InputPath(path.as_path())).unwrap();

        assert_eq!(graph.labels.len(), 5);
        assert_eq!(graph.edge_list.len(), 6);
        assert_eq!(graph.max_label, 2);
        assert_eq!(graph.max_degree, 3);
    }

    #[test]
    fn label_test() {
        let path = TEST_GRAPH.iter().collect::<PathBuf>();
        let graph = DotGraph::<usize, usize>::try_from(InputPath(path.as_path())).unwrap();

        assert_eq!(graph.max_label_frequency(), 2);
    }

    #[test]
    fn label_stats_test() {
        let path = TEST_GRAPH.iter().collect::<PathBuf>();
        let graph = DotGraph::<usize, usize>::try_from(InputPath(path.as_path())).unwrap();
        let graph = UndirectedCsrGraph::<usize, usize>::from((graph, CsrLayout::Sorted));

        let label_stats = LabelStats::from_graph(&graph);

        assert_eq!(label_stats.max_degree, 3);
        assert_eq!(label_stats.max_label, 2);
        assert_eq!(label_stats.max_label_frequency, 2);
        assert_eq!(label_stats.label_frequency[&0], 1);
        assert_eq!(label_stats.label_frequency[&1], 2);
        assert_eq!(label_stats.label_frequency[&2], 2);
    }

    #[test]
    fn neighbor_label_frequency_test() {
        let path = TEST_GRAPH.iter().collect::<PathBuf>();
        let graph = DotGraph::<usize, usize>::try_from(InputPath(path.as_path())).unwrap();
        let graph = UndirectedCsrGraph::<usize, usize>::from((graph, CsrLayout::Sorted));

        let nlf = NeighborLabelFrequencies::from_graph(&graph);

        assert_eq!(nlf.neighbor_frequency(0).get(0), None);
        assert_eq!(nlf.neighbor_frequency(0).get(1), Some(1));
        assert_eq!(nlf.neighbor_frequency(0).get(2), Some(1));

        assert_eq!(nlf.neighbor_frequency(1).get(0), Some(1));
        assert_eq!(nlf.neighbor_frequency(1).get(1), Some(1));
        assert_eq!(nlf.neighbor_frequency(1).get(2), Some(1));

        assert_eq!(nlf.neighbor_frequency(2).get(0), Some(1));
        assert_eq!(nlf.neighbor_frequency(2).get(1), Some(1));
        assert_eq!(nlf.neighbor_frequency(2).get(2), Some(1));

        assert_eq!(nlf.neighbor_frequency(3).get(0), None);
        assert_eq!(nlf.neighbor_frequency(3).get(1), Some(1));
        assert_eq!(nlf.neighbor_frequency(3).get(2), Some(1));

        assert_eq!(nlf.neighbor_frequency(4).get(0), None);
        assert_eq!(nlf.neighbor_frequency(4).get(1), Some(1));
        assert_eq!(nlf.neighbor_frequency(4).get(2), Some(1));
    }

    #[test]
    fn node_label_index_test() {
        let path = TEST_GRAPH.iter().collect::<PathBuf>();
        let graph = DotGraph::<usize, usize>::try_from(InputPath(path.as_path())).unwrap();
        let graph = UndirectedCsrGraph::<usize, usize>::from((graph, CsrLayout::Sorted));
        let label_stats = LabelStats::from_graph(&graph);

        let idx = NodeLabelIndex::from_stats(graph.node_count(), &label_stats, |node| {
            *graph.node_value(node)
        });

        assert_eq!(idx.nodes(0), &[0]);
        assert_eq!(idx.nodes(1), &[1, 3]);
        assert_eq!(idx.nodes(2), &[2, 4]);
    }
}
