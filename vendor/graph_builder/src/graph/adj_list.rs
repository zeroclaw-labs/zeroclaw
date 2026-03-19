use crate::{
    index::Idx, prelude::Direction, prelude::Edges, prelude::NodeValues as NodeValuesTrait,
    CsrLayout, DirectedDegrees, DirectedNeighbors, DirectedNeighborsWithValues, Graph, Target,
    UndirectedDegrees, UndirectedNeighbors, UndirectedNeighborsWithValues,
};
use crate::{EdgeMutation, EdgeMutationWithValues};

use log::info;
use std::sync::{RwLock, RwLockReadGuard};
use std::time::Instant;

use crate::graph::csr::NodeValues;
use rayon::prelude::*;

#[derive(Debug)]
pub struct AdjacencyList<NI, EV> {
    edges: Vec<RwLock<Vec<Target<NI, EV>>>>,
    layout: CsrLayout,
}

const _: () = {
    const fn is_send<T: Send>() {}
    const fn is_sync<T: Sync>() {}

    is_send::<AdjacencyList<u64, ()>>();
    is_sync::<AdjacencyList<u64, ()>>();
};

impl<NI: Idx, EV> AdjacencyList<NI, EV> {
    pub fn new(edges: Vec<Vec<Target<NI, EV>>>) -> Self {
        Self::with_layout(edges, CsrLayout::Unsorted)
    }

    pub fn with_layout(edges: Vec<Vec<Target<NI, EV>>>, layout: CsrLayout) -> Self {
        let edges = edges.into_iter().map(RwLock::new).collect::<_>();
        Self { edges, layout }
    }

    #[inline]
    pub(crate) fn node_count(&self) -> NI {
        NI::new(self.edges.len())
    }

    #[inline]
    pub(crate) fn edge_count(&self) -> NI
    where
        NI: Send + Sync,
        EV: Send + Sync,
    {
        NI::new(self.edges.par_iter().map(|v| v.read().unwrap().len()).sum())
    }

    #[inline]
    pub(crate) fn degree(&self, node: NI) -> NI {
        NI::new(self.edges[node.index()].read().unwrap().len())
    }

    #[inline]
    pub(crate) fn insert(&self, source: NI, target: Target<NI, EV>) {
        let mut edges = self.edges[source.index()].write().unwrap();
        Self::apply_layout(self.layout, &mut edges, target);
    }

    #[inline]
    pub(crate) fn insert_mut(&mut self, source: NI, target: Target<NI, EV>) {
        let edges = self.edges[source.index()].get_mut().unwrap();
        Self::apply_layout(self.layout, edges, target);
    }

    #[inline]
    fn check_bounds(&self, node: NI) -> Result<(), crate::Error> {
        if node >= self.node_count() {
            return Err(crate::Error::MissingNode {
                node: format!("{}", node.index()),
            });
        };
        Ok(())
    }

    #[inline]
    fn apply_layout(layout: CsrLayout, edges: &mut Vec<Target<NI, EV>>, target: Target<NI, EV>) {
        match layout {
            CsrLayout::Sorted => match edges.binary_search(&target) {
                Ok(i) => edges.insert(i, target),
                Err(i) => edges.insert(i, target),
            },
            CsrLayout::Unsorted => edges.push(target),
            CsrLayout::Deduplicated => match edges.binary_search(&target) {
                Ok(_) => {}
                Err(i) => edges.insert(i, target),
            },
        };
    }
}

#[derive(Debug)]
pub struct Targets<'slice, NI: Idx> {
    targets: RwLockReadGuard<'slice, Vec<Target<NI, ()>>>,
}

impl<'slice, NI: Idx> Targets<'slice, NI> {
    pub fn as_slice(&self) -> &'slice [NI] {
        assert_eq!(
            std::mem::size_of::<Target<NI, ()>>(),
            std::mem::size_of::<NI>()
        );
        assert_eq!(
            std::mem::align_of::<Target<NI, ()>>(),
            std::mem::align_of::<NI>()
        );
        // SAFETY: The types Target<T, ()> and T are verified to have the same
        //         size and alignment.
        //         We can upcast the lifetime since the MutexGuard
        //         is not exposed, so it is not possible to deref mutable.
        unsafe { std::slice::from_raw_parts(self.targets.as_ptr().cast(), self.targets.len()) }
    }
}

pub struct TargetsIter<'slice, NI: Idx> {
    _targets: Targets<'slice, NI>,
    slice: std::slice::Iter<'slice, NI>,
}

impl<'slice, NI: Idx> TargetsIter<'slice, NI> {
    pub fn as_slice(&self) -> &'slice [NI] {
        self.slice.as_slice()
    }
}

impl<'slice, NI: Idx> IntoIterator for Targets<'slice, NI> {
    type Item = &'slice NI;

    type IntoIter = TargetsIter<'slice, NI>;

    fn into_iter(self) -> Self::IntoIter {
        let slice = self.as_slice();
        TargetsIter {
            _targets: self,
            slice: slice.iter(),
        }
    }
}

impl<'slice, NI: Idx> Iterator for TargetsIter<'slice, NI> {
    type Item = &'slice NI;

    fn next(&mut self) -> Option<Self::Item> {
        self.slice.next()
    }
}

impl<NI: Idx> AdjacencyList<NI, ()> {
    #[inline]
    pub(crate) fn targets(&self, node: NI) -> Targets<'_, NI> {
        let targets = self.edges[node.index()].read().unwrap();

        Targets { targets }
    }
}

#[derive(Debug)]
pub struct TargetsWithValues<'slice, NI: Idx, EV> {
    targets: RwLockReadGuard<'slice, Vec<Target<NI, EV>>>,
}

impl<'slice, NI: Idx, EV> TargetsWithValues<'slice, NI, EV> {
    pub fn as_slice(&self) -> &'slice [Target<NI, EV>] {
        // SAFETY: We can upcast the lifetime since the MutexGuard
        // is not exposed, so it is not possible to deref mutable.
        unsafe { std::slice::from_raw_parts(self.targets.as_ptr(), self.targets.len()) }
    }
}

pub struct TargetsWithValuesIter<'slice, NI: Idx, EV> {
    _targets: TargetsWithValues<'slice, NI, EV>,
    slice: std::slice::Iter<'slice, Target<NI, EV>>,
}

impl<'slice, NI: Idx, EV> TargetsWithValuesIter<'slice, NI, EV> {
    pub fn as_slice(&self) -> &'slice [Target<NI, EV>] {
        self.slice.as_slice()
    }
}

impl<'slice, NI: Idx, EV> IntoIterator for TargetsWithValues<'slice, NI, EV> {
    type Item = &'slice Target<NI, EV>;

    type IntoIter = TargetsWithValuesIter<'slice, NI, EV>;

    fn into_iter(self) -> Self::IntoIter {
        let slice = self.as_slice();
        TargetsWithValuesIter {
            _targets: self,
            slice: slice.iter(),
        }
    }
}

impl<'slice, NI: Idx, EV> Iterator for TargetsWithValuesIter<'slice, NI, EV> {
    type Item = &'slice Target<NI, EV>;

    fn next(&mut self) -> Option<Self::Item> {
        self.slice.next()
    }
}

impl<NI: Idx, EV> AdjacencyList<NI, EV> {
    #[inline]
    pub(crate) fn targets_with_values(&self, node: NI) -> TargetsWithValues<'_, NI, EV> {
        TargetsWithValues {
            targets: self.edges[node.index()].read().unwrap(),
        }
    }
}

impl<NI, EV, E> From<(&'_ E, NI, Direction, CsrLayout)> for AdjacencyList<NI, EV>
where
    NI: Idx,
    EV: Copy + Send + Sync,
    E: Edges<NI = NI, EV = EV>,
{
    fn from(
        (edge_list, node_count, direction, csr_layout): (&'_ E, NI, Direction, CsrLayout),
    ) -> Self {
        let start = Instant::now();
        let thread_safe_vec = edge_list
            .degrees(node_count, direction)
            .into_par_iter()
            .map(|degree| RwLock::new(Vec::with_capacity(degree.into_inner().index())))
            .collect::<Vec<_>>();
        info!("Initialized adjacency list in {:?}", start.elapsed());

        let start = Instant::now();
        edge_list.edges().for_each(|(s, t, v)| {
            if matches!(direction, Direction::Outgoing | Direction::Undirected) {
                thread_safe_vec[s.index()]
                    .write()
                    .unwrap()
                    .push(Target::new(t, v));
            }
            if matches!(direction, Direction::Incoming | Direction::Undirected) {
                thread_safe_vec[t.index()]
                    .write()
                    .unwrap()
                    .push(Target::new(s, v));
            }
        });
        info!("Grouped edge tuples in {:?}", start.elapsed());

        let start = Instant::now();
        let mut edges = Vec::with_capacity(node_count.index());
        thread_safe_vec
            .into_par_iter()
            .map(|list| {
                let mut list = list.into_inner().unwrap();

                match csr_layout {
                    CsrLayout::Sorted => list.sort_unstable_by_key(|t| t.target),
                    CsrLayout::Unsorted => {}
                    CsrLayout::Deduplicated => {
                        list.sort_unstable_by_key(|t| t.target);
                        list.dedup_by_key(|t| t.target)
                    }
                }

                list
            })
            .collect_into_vec(&mut edges);

        info!(
            "Applied list layout and finalized edge list in {:?}",
            start.elapsed()
        );

        AdjacencyList::with_layout(edges, csr_layout)
    }
}

pub struct DirectedALGraph<NI: Idx, NV = (), EV = ()> {
    node_values: NodeValues<NV>,
    al_out: AdjacencyList<NI, EV>,
    al_inc: AdjacencyList<NI, EV>,
}

impl<NI: Idx, NV, EV> DirectedALGraph<NI, NV, EV>
where
    NV: Send + Sync,
    EV: Send + Sync,
{
    pub fn new(
        node_values: NodeValues<NV>,
        al_out: AdjacencyList<NI, EV>,
        al_inc: AdjacencyList<NI, EV>,
    ) -> Self {
        let g = Self {
            node_values,
            al_out,
            al_inc,
        };

        info!(
            "Created directed graph (node_count = {:?}, edge_count = {:?})",
            g.node_count(),
            g.edge_count()
        );

        g
    }
}

impl<NI: Idx, NV, EV> Graph<NI> for DirectedALGraph<NI, NV, EV>
where
    NV: Send + Sync,
    EV: Send + Sync,
{
    delegate::delegate! {
        to self.al_out {
            fn node_count(&self) -> NI;
            fn edge_count(&self) -> NI;
        }
    }
}

impl<NI: Idx, NV, EV> NodeValuesTrait<NI, NV> for DirectedALGraph<NI, NV, EV> {
    fn node_value(&self, node: NI) -> &NV {
        &self.node_values.0[node.index()]
    }
}

impl<NI: Idx, NV, EV> DirectedDegrees<NI> for DirectedALGraph<NI, NV, EV> {
    fn out_degree(&self, node: NI) -> NI {
        self.al_out.degree(node)
    }

    fn in_degree(&self, node: NI) -> NI {
        self.al_inc.degree(node)
    }
}

impl<NI: Idx, NV> DirectedNeighbors<NI> for DirectedALGraph<NI, NV, ()> {
    type NeighborsIterator<'a>
        = TargetsIter<'a, NI>
    where
        NV: 'a;

    fn out_neighbors(&self, node: NI) -> Self::NeighborsIterator<'_> {
        self.al_out.targets(node).into_iter()
    }

    fn in_neighbors(&self, node: NI) -> Self::NeighborsIterator<'_> {
        self.al_inc.targets(node).into_iter()
    }
}

impl<NI: Idx, NV, EV> DirectedNeighborsWithValues<NI, EV> for DirectedALGraph<NI, NV, EV> {
    type NeighborsIterator<'a>
        = TargetsWithValuesIter<'a, NI, EV>
    where
        NV: 'a,
        EV: 'a;

    fn out_neighbors_with_values(&self, node: NI) -> Self::NeighborsIterator<'_> {
        self.al_out.targets_with_values(node).into_iter()
    }

    fn in_neighbors_with_values(&self, node: NI) -> Self::NeighborsIterator<'_> {
        self.al_inc.targets_with_values(node).into_iter()
    }
}

impl<NI: Idx, NV> EdgeMutation<NI> for DirectedALGraph<NI, NV> {
    fn add_edge(&self, source: NI, target: NI) -> Result<(), crate::Error> {
        self.add_edge_with_value(source, target, ())
    }

    fn add_edge_mut(&mut self, source: NI, target: NI) -> Result<(), crate::Error> {
        self.add_edge_with_value_mut(source, target, ())
    }
}

impl<NI: Idx, NV, EV: Copy> EdgeMutationWithValues<NI, EV> for DirectedALGraph<NI, NV, EV> {
    fn add_edge_with_value(&self, source: NI, target: NI, value: EV) -> Result<(), crate::Error> {
        self.al_out.check_bounds(source)?;
        self.al_inc.check_bounds(target)?;
        self.al_out.insert(source, Target::new(target, value));
        self.al_inc.insert(target, Target::new(source, value));

        Ok(())
    }

    fn add_edge_with_value_mut(
        &mut self,
        source: NI,
        target: NI,
        value: EV,
    ) -> Result<(), crate::Error> {
        self.al_out.check_bounds(source)?;
        self.al_inc.check_bounds(target)?;
        self.al_out.insert_mut(source, Target::new(target, value));
        self.al_inc.insert_mut(target, Target::new(source, value));

        Ok(())
    }
}

impl<NI, EV, E> From<(E, CsrLayout)> for DirectedALGraph<NI, (), EV>
where
    NI: Idx,
    EV: Copy + Send + Sync,
    E: Edges<NI = NI, EV = EV>,
{
    fn from((edge_list, csr_layout): (E, CsrLayout)) -> Self {
        info!("Creating directed graph");
        let node_count = edge_list.max_node_id() + NI::new(1);
        let node_values = NodeValues::new(vec![(); node_count.index()]);

        let start = Instant::now();
        let al_out = AdjacencyList::from((&edge_list, node_count, Direction::Outgoing, csr_layout));
        info!("Created outgoing adjacency list in {:?}", start.elapsed());

        let start = Instant::now();
        let al_inc = AdjacencyList::from((&edge_list, node_count, Direction::Incoming, csr_layout));
        info!("Created incoming adjacency list in {:?}", start.elapsed());

        DirectedALGraph::new(node_values, al_out, al_inc)
    }
}

impl<NI, NV, EV, E> From<(NodeValues<NV>, E, CsrLayout)> for DirectedALGraph<NI, NV, EV>
where
    NI: Idx,
    NV: Send + Sync,
    EV: Copy + Send + Sync,
    E: Edges<NI = NI, EV = EV>,
{
    fn from((node_values, edge_list, csr_layout): (NodeValues<NV>, E, CsrLayout)) -> Self {
        info!("Creating directed graph");
        let node_count = edge_list.max_node_id() + NI::new(1);

        let start = Instant::now();
        let al_out = AdjacencyList::from((&edge_list, node_count, Direction::Outgoing, csr_layout));
        info!("Created outgoing adjacency list in {:?}", start.elapsed());

        let start = Instant::now();
        let al_inc = AdjacencyList::from((&edge_list, node_count, Direction::Incoming, csr_layout));
        info!("Created incoming adjacency list in {:?}", start.elapsed());

        DirectedALGraph::new(node_values, al_out, al_inc)
    }
}

pub struct UndirectedALGraph<NI: Idx, NV = (), EV = ()> {
    node_values: NodeValues<NV>,
    al: AdjacencyList<NI, EV>,
}

impl<NI: Idx, NV, EV> UndirectedALGraph<NI, NV, EV>
where
    NV: Send + Sync,
    EV: Send + Sync,
{
    pub fn new(node_values: NodeValues<NV>, al: AdjacencyList<NI, EV>) -> Self {
        let g = Self { node_values, al };

        info!(
            "Created undirected graph (node_count = {:?}, edge_count = {:?})",
            g.node_count(),
            g.edge_count()
        );

        g
    }
}

impl<NI: Idx, NV, EV> Graph<NI> for UndirectedALGraph<NI, NV, EV>
where
    NV: Send + Sync,
    EV: Send + Sync,
{
    fn node_count(&self) -> NI {
        self.al.node_count()
    }

    fn edge_count(&self) -> NI {
        self.al.edge_count() / NI::new(2)
    }
}

impl<NI: Idx, NV, EV> NodeValuesTrait<NI, NV> for UndirectedALGraph<NI, NV, EV> {
    fn node_value(&self, node: NI) -> &NV {
        &self.node_values.0[node.index()]
    }
}

impl<NI: Idx, NV, EV> UndirectedDegrees<NI> for UndirectedALGraph<NI, NV, EV> {
    fn degree(&self, node: NI) -> NI {
        self.al.degree(node)
    }
}

impl<NI: Idx, NV> UndirectedNeighbors<NI> for UndirectedALGraph<NI, NV, ()> {
    type NeighborsIterator<'a>
        = TargetsIter<'a, NI>
    where
        NV: 'a;

    fn neighbors(&self, node: NI) -> Self::NeighborsIterator<'_> {
        self.al.targets(node).into_iter()
    }
}

impl<NI: Idx, NV, EV> UndirectedNeighborsWithValues<NI, EV> for UndirectedALGraph<NI, NV, EV> {
    type NeighborsIterator<'a>
        = TargetsWithValuesIter<'a, NI, EV>
    where
        NV: 'a,
        EV: 'a;

    fn neighbors_with_values(&self, node: NI) -> Self::NeighborsIterator<'_> {
        self.al.targets_with_values(node).into_iter()
    }
}

impl<NI: Idx, NV> EdgeMutation<NI> for UndirectedALGraph<NI, NV, ()> {
    fn add_edge(&self, source: NI, target: NI) -> Result<(), crate::Error> {
        self.add_edge_with_value(source, target, ())
    }

    fn add_edge_mut(&mut self, source: NI, target: NI) -> Result<(), crate::Error> {
        self.add_edge_with_value_mut(source, target, ())
    }
}

impl<NI: Idx, NV, EV: Copy> EdgeMutationWithValues<NI, EV> for UndirectedALGraph<NI, NV, EV> {
    fn add_edge_with_value(&self, source: NI, target: NI, value: EV) -> Result<(), crate::Error> {
        self.al.check_bounds(source)?;
        self.al.check_bounds(target)?;
        self.al.insert(source, Target::new(target, value));
        self.al.insert(target, Target::new(source, value));

        Ok(())
    }

    fn add_edge_with_value_mut(
        &mut self,
        source: NI,
        target: NI,
        value: EV,
    ) -> Result<(), crate::Error> {
        self.al.check_bounds(source)?;
        self.al.check_bounds(target)?;
        self.al.insert_mut(source, Target::new(target, value));
        self.al.insert_mut(target, Target::new(source, value));

        Ok(())
    }
}

impl<NI, EV, E> From<(E, CsrLayout)> for UndirectedALGraph<NI, (), EV>
where
    NI: Idx,
    EV: Copy + Send + Sync,
    E: Edges<NI = NI, EV = EV>,
{
    fn from((edge_list, csr_layout): (E, CsrLayout)) -> Self {
        info!("Creating undirected graph");
        let node_count = edge_list.max_node_id() + NI::new(1);
        let node_values = NodeValues::new(vec![(); node_count.index()]);

        let start = Instant::now();
        let al = AdjacencyList::from((&edge_list, node_count, Direction::Undirected, csr_layout));
        info!("Created adjacency list in {:?}", start.elapsed());

        UndirectedALGraph::new(node_values, al)
    }
}

impl<NI, NV, EV, E> From<(NodeValues<NV>, E, CsrLayout)> for UndirectedALGraph<NI, NV, EV>
where
    NI: Idx,
    NV: Send + Sync,
    EV: Copy + Send + Sync,
    E: Edges<NI = NI, EV = EV>,
{
    fn from((node_values, edge_list, csr_layout): (NodeValues<NV>, E, CsrLayout)) -> Self {
        info!("Creating undirected graph");
        let node_count = edge_list.max_node_id() + NI::new(1);

        let start = Instant::now();
        let al = AdjacencyList::from((&edge_list, node_count, Direction::Undirected, csr_layout));
        info!("Created adjacency list in {:?}", start.elapsed());

        UndirectedALGraph::new(node_values, al)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::prelude::EdgeList;
    use crate::GraphBuilder;
    use tap::prelude::*;

    #[test]
    fn empty_list() {
        let list = AdjacencyList::<u32, u32>::new(vec![]);
        assert_eq!(list.node_count(), 0);
        assert_eq!(list.edge_count(), 0);
    }

    #[test]
    fn degree() {
        let list = AdjacencyList::<u32, u32>::new(vec![
            /* node 0 */ vec![Target::new(1, 42)],
            /* node 1 */ vec![Target::new(0, 1337)],
        ]);
        assert_eq!(list.node_count(), 2);
        assert_eq!(list.edge_count(), 2);
        assert_eq!(list.degree(0), 1);
        assert_eq!(list.degree(1), 1);
    }

    #[test]
    fn targets_with_values() {
        let list = AdjacencyList::<u32, u32>::new(vec![
            /* node 0 */ vec![Target::new(1, 42)],
            /* node 1 */ vec![Target::new(0, 1337)],
        ]);

        assert_eq!(
            list.targets_with_values(0).as_slice(),
            &[Target::new(1, 42)]
        );
        assert_eq!(
            list.targets_with_values(1).as_slice(),
            &[Target::new(0, 1337)]
        );
    }

    #[test]
    fn targets() {
        let list = AdjacencyList::<u32, ()>::new(vec![
            /* node 0 */ vec![Target::new(1, ())],
            /* node 1 */ vec![Target::new(0, ())],
        ]);

        assert_eq!(list.targets(0).as_slice(), &[1]);
        assert_eq!(list.targets(1).as_slice(), &[0]);
    }

    #[test]
    fn from_edges_outgoing() {
        let edges = vec![(0, 1, 42), (0, 2, 1337), (1, 0, 43), (2, 0, 1338)];
        let edges = EdgeList::new(edges);
        let list =
            AdjacencyList::<u32, u32>::from((&edges, 3, Direction::Outgoing, CsrLayout::Unsorted));

        assert_eq!(
            list.targets_with_values(0)
                .into_iter()
                .collect::<Vec<_>>()
                .tap_mut(|v| v.sort_by_key(|t| t.target)),
            &[&Target::new(1, 42), &Target::new(2, 1337)]
        );
        assert_eq!(
            list.targets_with_values(1).as_slice(),
            &[Target::new(0, 84)]
        );
        assert_eq!(
            list.targets_with_values(2).as_slice(),
            &[Target::new(0, 1337)]
        );
    }

    #[test]
    fn from_edges_incoming() {
        let edges = vec![(0, 1, 42), (0, 2, 1337), (1, 0, 43), (2, 0, 1338)];
        let edges = EdgeList::new(edges);
        let list =
            AdjacencyList::<u32, u32>::from((&edges, 3, Direction::Incoming, CsrLayout::Unsorted));

        assert_eq!(
            list.targets_with_values(0)
                .into_iter()
                .collect::<Vec<_>>()
                .tap_mut(|v| v.sort_by_key(|t| t.target)),
            &[&Target::new(1, 42), &Target::new(2, 1337)]
        );
        assert_eq!(
            list.targets_with_values(1).as_slice(),
            &[Target::new(0, 42)]
        );
        assert_eq!(
            list.targets_with_values(2).as_slice(),
            &[Target::new(0, 1337)]
        );
    }

    #[test]
    fn from_edges_undirected() {
        let edges = vec![(0, 1, 42), (0, 2, 1337), (1, 0, 43), (2, 0, 1338)];
        let edges = EdgeList::new(edges);
        let list = AdjacencyList::<u32, u32>::from((
            &edges,
            3,
            Direction::Undirected,
            CsrLayout::Unsorted,
        ));

        assert_eq!(
            list.targets_with_values(0)
                .into_iter()
                .collect::<Vec<_>>()
                .tap_mut(|v| v.sort_by_key(|t| t.target)),
            &[
                &Target::new(1, 42),
                &Target::new(1, 43),
                &Target::new(2, 1337),
                &Target::new(2, 1338)
            ]
        );
        assert_eq!(
            list.targets_with_values(1)
                .into_iter()
                .collect::<Vec<_>>()
                .tap_mut(|v| v.sort_by_key(|t| t.target)),
            &[&Target::new(0, 42), &Target::new(0, 43)]
        );
        assert_eq!(
            list.targets_with_values(2)
                .into_iter()
                .collect::<Vec<_>>()
                .tap_mut(|v| v.sort_by_key(|t| t.target)),
            &[&Target::new(0, 1337), &Target::new(0, 1338)]
        );
    }

    #[test]
    fn from_edges_sorted() {
        let edges = vec![
            (0, 1, ()),
            (0, 3, ()),
            (0, 2, ()),
            (1, 3, ()),
            (1, 2, ()),
            (1, 0, ()),
        ];
        let edges = EdgeList::new(edges);
        let list =
            AdjacencyList::<u32, ()>::from((&edges, 3, Direction::Outgoing, CsrLayout::Sorted));

        assert_eq!(list.targets(0).as_slice(), &[1, 2, 3]);
        assert_eq!(list.targets(1).as_slice(), &[0, 2, 3]);
    }

    #[test]
    fn from_edges_deduplicated() {
        let edges = vec![
            (0, 1, ()),
            (0, 3, ()),
            (0, 3, ()),
            (0, 2, ()),
            (0, 2, ()),
            (1, 3, ()),
            (1, 3, ()),
            (1, 2, ()),
            (1, 2, ()),
            (1, 0, ()),
            (1, 0, ()),
        ];
        let edges = EdgeList::new(edges);
        let list = AdjacencyList::<u32, ()>::from((
            &edges,
            3,
            Direction::Outgoing,
            CsrLayout::Deduplicated,
        ));

        assert_eq!(list.targets(0).as_slice(), &[1, 2, 3]);
        assert_eq!(list.targets(1).as_slice(), &[0, 2, 3]);
    }

    #[test]
    fn directed_al_graph() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Sorted)
            .edges([(0, 1), (0, 2), (1, 2)])
            .build::<DirectedALGraph<u32, ()>>();

        assert_eq!(g.node_count(), 3);
        assert_eq!(g.edge_count(), 3);
        assert_eq!(g.out_degree(0), 2);
        assert_eq!(g.out_neighbors(0).as_slice(), &[1, 2]);
        assert_eq!(g.in_degree(2), 2);
        assert_eq!(g.in_neighbors(2).as_slice(), &[0, 1]);
    }

    #[test]
    fn directed_al_graph_with_node_values() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Sorted)
            .edges([(0, 1), (0, 2), (1, 2)])
            .node_values(vec!["foo", "bar", "baz"])
            .build::<DirectedALGraph<u32, &str>>();

        assert_eq!(g.node_value(0), &"foo");
        assert_eq!(g.node_value(1), &"bar");
        assert_eq!(g.node_value(2), &"baz");
    }

    #[test]
    fn directed_al_graph_add_edge_unsorted() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Unsorted)
            .edges([(0, 2), (1, 2)])
            .build::<DirectedALGraph<u32>>();

        assert_eq!(g.out_neighbors(0).as_slice(), &[2]);
        g.add_edge(0, 1).expect("add edge failed");
        assert_eq!(g.out_neighbors(0).as_slice(), &[2, 1]);
        g.add_edge(0, 2).expect("add edge failed");
        assert_eq!(g.out_neighbors(0).as_slice(), &[2, 1, 2]);
    }

    #[test]
    fn directed_al_graph_add_edge_sorted() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Sorted)
            .edges([(0, 2), (1, 2)])
            .build::<DirectedALGraph<u32>>();

        assert_eq!(g.out_neighbors(0).as_slice(), &[2]);
        g.add_edge(0, 1).expect("add edge failed");
        assert_eq!(g.out_neighbors(0).as_slice(), &[1, 2]);
        g.add_edge(0, 1).expect("add edge failed");
        assert_eq!(g.out_neighbors(0).as_slice(), &[1, 1, 2]);
    }

    #[test]
    fn directed_al_graph_add_edge_deduplicated() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Deduplicated)
            .edges([(0, 2), (1, 2), (1, 3)])
            .build::<DirectedALGraph<u32>>();

        assert_eq!(g.out_neighbors(0).as_slice(), &[2]);
        g.add_edge(0, 1).expect("add edge failed");
        assert_eq!(g.out_neighbors(0).as_slice(), &[1, 2]);
        g.add_edge(0, 1).expect("add edge failed");
        assert_eq!(g.out_neighbors(0).as_slice(), &[1, 2]);
        g.add_edge(0, 3).expect("add edge failed");
        assert_eq!(g.out_neighbors(0).as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn directed_al_graph_add_edge_with_value() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Unsorted)
            .edges_with_values([(0, 2, 4.2), (1, 2, 13.37)])
            .build::<DirectedALGraph<u32, (), f32>>();

        assert_eq!(
            g.out_neighbors_with_values(0).as_slice(),
            &[Target::new(2, 4.2)]
        );
        g.add_edge_with_value(0, 1, 19.84).expect("add edge failed");
        assert_eq!(
            g.out_neighbors_with_values(0).as_slice(),
            &[Target::new(2, 4.2), Target::new(1, 19.84)]
        );
        g.add_edge_with_value(0, 2, 1.23).expect("add edge failed");
        assert_eq!(
            g.out_neighbors_with_values(0).as_slice(),
            &[
                Target::new(2, 4.2),
                Target::new(1, 19.84),
                Target::new(2, 1.23)
            ]
        );
    }

    #[test]
    fn directed_al_graph_add_edge_missing_node() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Unsorted)
            .edges([(0, 2), (1, 2)])
            .build::<DirectedALGraph<u32>>();

        let err = g.add_edge(0, 3).unwrap_err();

        assert!(matches!(err, crate::Error::MissingNode { node } if node == "3" ));
    }

    #[test]
    fn directed_al_graph_add_edge_parallel() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Unsorted)
            .edges([(0, 1), (0, 2), (0, 3)])
            .build::<DirectedALGraph<u32>>();

        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| g.add_edge(0, 1));
            }
        });

        assert_eq!(g.edge_count(), 7);
    }

    #[test]
    fn undirected_al_graph_add_edge_unsorted() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Unsorted)
            .edges([(0, 2), (1, 2)])
            .build::<UndirectedALGraph<u32>>();

        assert_eq!(g.neighbors(0).as_slice(), &[2]);
        assert_eq!(g.neighbors(1).as_slice(), &[2]);
        g.add_edge(0, 1).expect("add edge failed");
        assert_eq!(g.neighbors(0).as_slice(), &[2, 1]);
        assert_eq!(g.neighbors(1).as_slice(), &[2, 0]);
        g.add_edge(0, 2).expect("add edge failed");
        assert_eq!(g.neighbors(0).as_slice(), &[2, 1, 2]);
    }

    #[test]
    fn undirected_al_graph_add_edge_sorted() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Sorted)
            .edges([(0, 2), (1, 2)])
            .build::<UndirectedALGraph<u32>>();

        assert_eq!(g.neighbors(0).as_slice(), &[2]);
        assert_eq!(g.neighbors(1).as_slice(), &[2]);
        g.add_edge(0, 1).expect("add edge failed");
        assert_eq!(g.neighbors(0).as_slice(), &[1, 2]);
        assert_eq!(g.neighbors(1).as_slice(), &[0, 2]);
        g.add_edge(0, 1).expect("add edge failed");
        assert_eq!(g.neighbors(0).as_slice(), &[1, 1, 2]);
    }

    #[test]
    fn undirected_al_graph_add_edge_deduplicated() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Deduplicated)
            .edges([(0, 2), (1, 2), (1, 3)])
            .build::<UndirectedALGraph<u32>>();

        assert_eq!(g.neighbors(0).as_slice(), &[2]);
        assert_eq!(g.neighbors(1).as_slice(), &[2, 3]);
        g.add_edge(0, 1).expect("add edge failed");
        assert_eq!(g.neighbors(0).as_slice(), &[1, 2]);
        assert_eq!(g.neighbors(1).as_slice(), &[0, 2, 3]);
        g.add_edge(0, 1).expect("add edge failed");
        assert_eq!(g.neighbors(0).as_slice(), &[1, 2]);
        assert_eq!(g.neighbors(1).as_slice(), &[0, 2, 3]);
        g.add_edge(0, 3).expect("add edge failed");
        assert_eq!(g.neighbors(0).as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn undirected_al_graph_add_edge_with_value() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Unsorted)
            .edges_with_values([(0, 2, 4.2), (1, 2, 13.37)])
            .build::<UndirectedALGraph<u32, (), f32>>();

        assert_eq!(
            g.neighbors_with_values(0).as_slice(),
            &[Target::new(2, 4.2)]
        );
        assert_eq!(
            g.neighbors_with_values(1).as_slice(),
            &[Target::new(2, 13.37)]
        );
        g.add_edge_with_value(0, 1, 19.84).expect("add edge failed");
        assert_eq!(
            g.neighbors_with_values(0).as_slice(),
            &[Target::new(2, 4.2), Target::new(1, 19.84)]
        );
        assert_eq!(
            g.neighbors_with_values(1).as_slice(),
            &[Target::new(2, 13.37), Target::new(0, 19.84)]
        );
        g.add_edge_with_value(0, 2, 1.23).expect("add edge failed");
        assert_eq!(
            g.neighbors_with_values(0).as_slice(),
            &[
                Target::new(2, 4.2),
                Target::new(1, 19.84),
                Target::new(2, 1.23)
            ]
        );
    }

    #[test]
    fn undirected_al_graph_add_edge_missing_node() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Unsorted)
            .edges([(0, 2), (1, 2)])
            .build::<UndirectedALGraph<u32>>();

        let err = g.add_edge(0, 3).unwrap_err();

        assert!(matches!(err, crate::Error::MissingNode { node } if node == "3" ));
    }

    #[test]
    fn undirected_al_graph_add_edge_parallel() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Unsorted)
            .edges([(0, 1), (0, 2), (0, 3)])
            .build::<UndirectedALGraph<u32>>();

        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| g.add_edge(0, 1));
            }
        });

        assert_eq!(g.edge_count(), 7);
    }
    #[test]
    fn undirected_al_graph() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Sorted)
            .edges([(0, 1), (0, 2), (1, 2)])
            .build::<UndirectedALGraph<u32, ()>>();

        assert_eq!(g.node_count(), 3);
        assert_eq!(g.edge_count(), 3);
        assert_eq!(g.degree(0), 2);
        assert_eq!(g.degree(2), 2);
        assert_eq!(g.neighbors(0).as_slice(), &[1, 2]);
        assert_eq!(g.neighbors(2).as_slice(), &[0, 1]);
    }

    #[test]
    fn undirected_al_graph_cycle() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Sorted)
            .edges([(0, 1), (1, 0)])
            .build::<UndirectedALGraph<u32, ()>>();

        assert_eq!(g.node_count(), 2);
        assert_eq!(g.edge_count(), 2);
        assert_eq!(g.degree(0), 2);
        assert_eq!(g.degree(1), 2);
        assert_eq!(g.neighbors(0).as_slice(), &[1, 1]);
        assert_eq!(g.neighbors(1).as_slice(), &[0, 0]);
    }

    #[test]
    fn undirected_al_graph_with_node_values() {
        let g = GraphBuilder::new()
            .csr_layout(CsrLayout::Sorted)
            .edges([(0, 1), (0, 2), (1, 2)])
            .node_values(vec!["foo", "bar", "baz"])
            .build::<UndirectedALGraph<u32, &str>>();

        assert_eq!(g.node_value(0), &"foo");
        assert_eq!(g.node_value(1), &"bar");
        assert_eq!(g.node_value(2), &"baz");
    }
}
