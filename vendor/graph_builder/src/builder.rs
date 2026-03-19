use std::{convert::TryFrom, marker::PhantomData};

use crate::{
    graph::csr::{CsrLayout, NodeValues},
    index::Idx,
    input::{edgelist::EdgeList, InputCapabilities, InputPath},
    prelude::edgelist::{EdgeIterator, EdgeWithValueIterator},
    Error,
};
use std::path::Path as StdPath;

pub struct Uninitialized {
    csr_layout: CsrLayout,
}

pub struct FromEdges<NI, Edges>
where
    NI: Idx,
    Edges: IntoIterator<Item = (NI, NI)>,
{
    csr_layout: CsrLayout,
    edges: Edges,
    _node: PhantomData<NI>,
}

pub struct FromEdgeListAndNodeValues<NI, NV, EV>
where
    NI: Idx,
{
    csr_layout: CsrLayout,
    node_values: NodeValues<NV>,
    edge_list: EdgeList<NI, EV>,
}

pub struct FromEdgesWithValues<NI, Edges, EV>
where
    NI: Idx,
    Edges: IntoIterator<Item = (NI, NI, EV)>,
{
    csr_layout: CsrLayout,
    edges: Edges,
    _node: PhantomData<NI>,
}

#[cfg(feature = "gdl")]
#[cfg_attr(all(feature = "gdl", has_doc_cfg), doc(cfg(feature = "gdl")))]
pub struct FromGdlString<NI>
where
    NI: Idx,
{
    csr_layout: CsrLayout,
    gdl: String,
    _node: PhantomData<NI>,
}

#[cfg(feature = "gdl")]
#[cfg_attr(all(feature = "gdl", has_doc_cfg), doc(cfg(feature = "gdl")))]
pub struct FromGdlGraph<'a, NI>
where
    NI: Idx,
{
    csr_layout: CsrLayout,
    gdl_graph: &'a gdl::Graph,
    _node: PhantomData<NI>,
}

pub struct FromInput<NI, P, Format>
where
    P: AsRef<StdPath>,
    NI: Idx,
    Format: InputCapabilities<NI>,
    Format::GraphInput: TryFrom<InputPath<P>>,
{
    csr_layout: CsrLayout,
    _idx: PhantomData<NI>,
    _path: PhantomData<P>,
    _format: PhantomData<Format>,
}

pub struct FromPath<NI, P, Format>
where
    P: AsRef<StdPath>,
    NI: Idx,
    Format: InputCapabilities<NI>,
    Format::GraphInput: TryFrom<InputPath<P>>,
{
    csr_layout: CsrLayout,
    path: P,
    _idx: PhantomData<NI>,
    _format: PhantomData<Format>,
}

/// A builder to create graphs in a type-safe way.
///
/// The builder implementation uses different states to allow staged building of
/// graphs. Each individual state enables stage-specific methods on the builder.
///
/// # Examples
///
/// Create a directed graph from a vec of edges:
///
/// ```
/// use graph_builder::prelude::*;
///
/// let graph: DirectedCsrGraph<usize> = GraphBuilder::new()
///     .edges(vec![(0, 1), (0, 2), (1, 2), (1, 3), (2, 3)])
///     .build();
///
/// assert_eq!(graph.node_count(), 4);
/// ```
///
/// Create an undirected graph from a vec of edges:
///
/// ```
/// use graph_builder::prelude::*;
///
/// let graph: UndirectedCsrGraph<usize> = GraphBuilder::new()
///     .edges(vec![(0, 1), (0, 2), (1, 2), (1, 3), (2, 3)])
///     .build();
///
/// assert_eq!(graph.node_count(), 4);
/// ```
pub struct GraphBuilder<State> {
    state: State,
}

impl Default for GraphBuilder<Uninitialized> {
    fn default() -> Self {
        GraphBuilder::new()
    }
}

impl GraphBuilder<Uninitialized> {
    /// Creates a new builder
    pub fn new() -> Self {
        Self {
            state: Uninitialized {
                csr_layout: CsrLayout::default(),
            },
        }
    }

    /// Sets the [`CsrLayout`] to use during CSR construction.
    ///
    /// # Examples
    ///
    /// Store the neighbors sorted:
    ///
    /// ```
    /// use graph_builder::prelude::*;
    ///
    /// let graph: UndirectedCsrGraph<usize> = GraphBuilder::new()
    ///     .csr_layout(CsrLayout::Sorted)
    ///     .edges(vec![(0, 7), (0, 3), (0, 3), (0, 1)])
    ///     .build();
    ///
    /// assert_eq!(graph.neighbors(0).copied().collect::<Vec<_>>(), &[1, 3, 3, 7]);
    /// ```
    ///
    /// Store the neighbors sorted and deduplicated:
    ///
    /// ```
    /// use graph_builder::prelude::*;
    ///
    /// let graph: UndirectedCsrGraph<usize> = GraphBuilder::new()
    ///     .csr_layout(CsrLayout::Deduplicated)
    ///     .edges(vec![(0, 7), (0, 3), (0, 3), (0, 1)])
    ///     .build();
    ///
    /// assert_eq!(graph.neighbors(0).copied().collect::<Vec<_>>(), &[1, 3, 7]);
    /// ```
    #[must_use]
    pub fn csr_layout(mut self, csr_layout: CsrLayout) -> Self {
        self.state.csr_layout = csr_layout;
        self
    }

    /// Create a graph from the given edge tuples.
    ///
    /// # Example
    ///
    /// ```
    /// use graph_builder::prelude::*;
    ///
    /// let graph: DirectedCsrGraph<usize> = GraphBuilder::new()
    ///     .edges(vec![(0, 1), (0, 2), (1, 2), (1, 3), (2, 3)])
    ///     .build();
    ///
    /// assert_eq!(graph.node_count(), 4);
    /// assert_eq!(graph.edge_count(), 5);
    /// ```
    pub fn edges<NI, Edges>(self, edges: Edges) -> GraphBuilder<FromEdges<NI, Edges>>
    where
        NI: Idx,
        Edges: IntoIterator<Item = (NI, NI)>,
    {
        GraphBuilder {
            state: FromEdges {
                csr_layout: self.state.csr_layout,
                edges,
                _node: PhantomData,
            },
        }
    }

    /// Create a graph from the given edge triplets.
    ///
    /// # Example
    ///
    /// ```
    /// use graph_builder::prelude::*;
    ///
    /// let graph: DirectedCsrGraph<usize, (), f32> = GraphBuilder::new()
    ///     .edges_with_values(vec![(0, 1, 0.1), (0, 2, 0.2), (1, 2, 0.3), (1, 3, 0.4), (2, 3, 0.5)])
    ///     .build();
    ///
    /// assert_eq!(graph.node_count(), 4);
    /// assert_eq!(graph.edge_count(), 5);
    /// ```
    pub fn edges_with_values<NI, Edges, EV>(
        self,
        edges: Edges,
    ) -> GraphBuilder<FromEdgesWithValues<NI, Edges, EV>>
    where
        NI: Idx,
        Edges: IntoIterator<Item = (NI, NI, EV)>,
    {
        GraphBuilder {
            state: FromEdgesWithValues {
                csr_layout: self.state.csr_layout,
                edges,
                _node: PhantomData,
            },
        }
    }

    /// Creates a graph using Graph Definition Language (GDL).
    ///
    /// Creating graphs from GDL is recommended for small graphs only, e.g.,
    /// during testing. The graph construction is **not** parallelized.
    ///
    /// See [GDL on crates.io](https://crates.io/crates/gdl) for more
    /// information on how to define graphs using GDL.
    ///
    /// # Example
    ///
    /// ```
    /// use graph_builder::prelude::*;
    ///
    /// let g: UndirectedCsrGraph<usize> = GraphBuilder::new()
    ///     .gdl_str::<usize, _>("(a)-->(),(a)-->()")
    ///     .build()
    ///     .unwrap();
    ///
    /// assert_eq!(g.node_count(), 3);
    /// assert_eq!(g.edge_count(), 2);
    /// ```
    ///
    /// One can also create weighted graphs using GDL. There needs to be exactly
    /// one edge property with the same type as specified for the edge value.
    /// The property key is not relevant.
    ///
    /// ```
    /// use graph_builder::prelude::*;
    ///
    /// let g: UndirectedCsrGraph<usize, (), f32> = GraphBuilder::new()
    ///     .csr_layout(CsrLayout::Sorted)
    ///     .gdl_str::<usize, _>("(a)-[{f: 0.42}]->(),(a)-[{f: 13.37}]->()")
    ///     .build()
    ///     .unwrap();
    ///
    /// assert_eq!(g.node_count(), 3);
    /// assert_eq!(g.edge_count(), 2);
    ///
    /// let mut neighbors = g.neighbors_with_values(0);
    /// assert_eq!(neighbors.next(), Some(&Target::new(1, 0.42)));
    /// assert_eq!(neighbors.next(), Some(&Target::new(2, 13.37)));
    /// assert_eq!(neighbors.next(), None);
    /// ```
    #[cfg(feature = "gdl")]
    #[cfg_attr(all(feature = "gdl", has_doc_cfg), doc(cfg(feature = "gdl")))]
    pub fn gdl_str<NI, S>(self, gdl: S) -> GraphBuilder<FromGdlString<NI>>
    where
        NI: Idx,
        S: Into<String>,
    {
        GraphBuilder {
            state: FromGdlString {
                csr_layout: self.state.csr_layout,
                gdl: gdl.into(),
                _node: PhantomData,
            },
        }
    }

    /// Creates a graph from an already constructed GDL graph.
    ///
    /// Creating graphs from GDL is recommended for small graphs only, e.g.,
    /// during testing. The graph construction is **not** parallelized.
    ///
    /// See [GDL on crates.io](https://crates.io/crates/gdl) for more
    /// information on how to define graphs using GDL.
    ///
    /// # Example
    ///
    /// ```
    /// use graph_builder::prelude::*;
    ///
    /// let gdl_graph = "(a)-->(),(a)-->()".parse::<::gdl::Graph>().unwrap();
    ///
    /// let g: DirectedCsrGraph<usize> = GraphBuilder::new()
    ///     .gdl_graph::<usize>(&gdl_graph)
    ///     .build()
    ///     .unwrap();
    ///
    /// assert_eq!(g.node_count(), 3);
    /// assert_eq!(g.edge_count(), 2);
    ///
    /// let id_a = gdl_graph.get_node("a").unwrap().id();
    ///
    /// assert_eq!(g.out_neighbors(id_a).len(), 2);
    /// ```
    #[cfg(feature = "gdl")]
    #[cfg_attr(all(feature = "gdl", has_doc_cfg), doc(cfg(feature = "gdl")))]
    pub fn gdl_graph<NI>(self, gdl_graph: &::gdl::Graph) -> GraphBuilder<FromGdlGraph<NI>>
    where
        NI: Idx,
    {
        GraphBuilder {
            state: FromGdlGraph {
                csr_layout: self.state.csr_layout,
                gdl_graph,
                _node: PhantomData,
            },
        }
    }

    /// Creates a graph by reading it from the given file format.
    ///
    /// # Examples
    ///
    /// Read a directed graph from an edge list file:
    ///
    /// ```
    /// use std::path::PathBuf;
    ///
    /// use graph_builder::prelude::*;
    ///
    /// let path = [env!("CARGO_MANIFEST_DIR"), "resources", "example.el"]
    ///     .iter()
    ///     .collect::<PathBuf>();
    ///
    /// let graph: DirectedCsrGraph<usize> = GraphBuilder::new()
    ///     .file_format(EdgeListInput::default())
    ///     .path(path)
    ///     .build()
    ///     .expect("loading failed");
    ///
    /// assert_eq!(graph.node_count(), 4);
    /// assert_eq!(graph.edge_count(), 5);
    /// ```
    pub fn file_format<Format, Path, NI>(
        self,
        _format: Format,
    ) -> GraphBuilder<FromInput<NI, Path, Format>>
    where
        Path: AsRef<StdPath>,
        NI: Idx,
        Format: InputCapabilities<NI>,
        Format::GraphInput: TryFrom<InputPath<Path>>,
    {
        GraphBuilder {
            state: FromInput {
                csr_layout: self.state.csr_layout,
                _idx: PhantomData,
                _path: PhantomData,
                _format: PhantomData,
            },
        }
    }
}

impl<NI, Edges> GraphBuilder<FromEdges<NI, Edges>>
where
    NI: Idx,
    Edges: IntoIterator<Item = (NI, NI)>,
{
    pub fn node_values<NV, I>(
        self,
        node_values: I,
    ) -> GraphBuilder<FromEdgeListAndNodeValues<NI, NV, ()>>
    where
        I: IntoIterator<Item = NV>,
    {
        let edge_list = EdgeList::from(EdgeIterator(self.state.edges));
        let node_values = node_values.into_iter().collect::<NodeValues<NV>>();

        GraphBuilder {
            state: FromEdgeListAndNodeValues {
                csr_layout: self.state.csr_layout,
                node_values,
                edge_list,
            },
        }
    }

    /// Build the graph from the given vec of edges.
    pub fn build<Graph>(self) -> Graph
    where
        Graph: From<(EdgeList<NI, ()>, CsrLayout)>,
    {
        Graph::from((
            EdgeList::from(EdgeIterator(self.state.edges)),
            self.state.csr_layout,
        ))
    }
}

impl<NI, Edges, EV> GraphBuilder<FromEdgesWithValues<NI, Edges, EV>>
where
    NI: Idx,
    EV: Sync,
    Edges: IntoIterator<Item = (NI, NI, EV)>,
{
    pub fn node_values<NV, I>(
        self,
        node_values: I,
    ) -> GraphBuilder<FromEdgeListAndNodeValues<NI, NV, EV>>
    where
        I: IntoIterator<Item = NV>,
    {
        let edge_list = EdgeList::from(EdgeWithValueIterator(self.state.edges));
        let node_values = node_values.into_iter().collect::<NodeValues<NV>>();

        GraphBuilder {
            state: FromEdgeListAndNodeValues {
                csr_layout: self.state.csr_layout,
                node_values,
                edge_list,
            },
        }
    }

    /// Build the graph from the given vec of edges.
    pub fn build<Graph>(self) -> Graph
    where
        Graph: From<(EdgeList<NI, EV>, CsrLayout)>,
    {
        Graph::from((
            EdgeList::new(self.state.edges.into_iter().collect()),
            self.state.csr_layout,
        ))
    }
}

impl<NI: Idx, NV, EV> GraphBuilder<FromEdgeListAndNodeValues<NI, NV, EV>> {
    pub fn build<Graph>(self) -> Graph
    where
        Graph: From<(NodeValues<NV>, EdgeList<NI, EV>, CsrLayout)>,
    {
        Graph::from((
            self.state.node_values,
            self.state.edge_list,
            self.state.csr_layout,
        ))
    }
}

#[cfg(feature = "gdl")]
#[cfg_attr(all(feature = "gdl", has_doc_cfg), doc(cfg(feature = "gdl")))]
impl<NI> GraphBuilder<FromGdlString<NI>>
where
    NI: Idx,
{
    /// Builds the graph from the given GDL string.
    pub fn build<Graph>(self) -> Result<Graph, Error>
    where
        Graph: From<(gdl::Graph, CsrLayout)>,
    {
        let gdl_graph = self.state.gdl.parse::<gdl::Graph>()?;
        let graph = Graph::from((gdl_graph, self.state.csr_layout));
        Ok(graph)
    }
}

#[cfg(feature = "gdl")]
#[cfg_attr(all(feature = "gdl", has_doc_cfg), doc(cfg(feature = "gdl")))]
impl<'a, NI> GraphBuilder<FromGdlGraph<'a, NI>>
where
    NI: Idx,
{
    /// Build the graph from the given GDL graph.
    pub fn build<Graph>(self) -> Result<Graph, Error>
    where
        Graph: From<(&'a gdl::Graph, CsrLayout)>,
    {
        Ok(Graph::from((self.state.gdl_graph, self.state.csr_layout)))
    }
}

impl<NI, Path, Format> GraphBuilder<FromInput<NI, Path, Format>>
where
    Path: AsRef<StdPath>,
    NI: Idx,
    Format: InputCapabilities<NI>,
    Format::GraphInput: TryFrom<InputPath<Path>>,
{
    /// Set the location where the graph is stored.
    pub fn path(self, path: Path) -> GraphBuilder<FromPath<NI, Path, Format>> {
        GraphBuilder {
            state: FromPath {
                csr_layout: self.state.csr_layout,
                path,
                _idx: PhantomData,
                _format: PhantomData,
            },
        }
    }
}

impl<NI, Path, Format> GraphBuilder<FromPath<NI, Path, Format>>
where
    Path: AsRef<StdPath>,
    NI: Idx,
    Format: InputCapabilities<NI>,
    Format::GraphInput: TryFrom<InputPath<Path>>,
    crate::Error: From<<Format::GraphInput as TryFrom<InputPath<Path>>>::Error>,
{
    /// Build the graph from the given input format and path.
    pub fn build<Graph>(self) -> Result<Graph, Error>
    where
        Graph: TryFrom<(Format::GraphInput, CsrLayout)>,
        crate::Error: From<Graph::Error>,
    {
        let input = Format::GraphInput::try_from(InputPath(self.state.path))?;
        let graph = Graph::try_from((input, self.state.csr_layout))?;

        Ok(graph)
    }
}
