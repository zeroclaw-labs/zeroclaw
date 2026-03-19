#![cfg_attr(has_maybe_uninit_write_slice, feature(maybe_uninit_write_slice))]
#![cfg_attr(has_new_uninit, feature(new_uninit))]
#![cfg_attr(has_doc_cfg, feature(doc_cfg))]
#![cfg_attr(has_slice_partition_dedup, feature(slice_partition_dedup))]

//! A library that can be used as a building block for high-performant graph
//! algorithms.
//!
//! Graph provides implementations for directed and undirected graphs. Graphs
//! can be created programatically or read from custom input formats in a
//! type-safe way. The library uses [rayon](https://github.com/rayon-rs/rayon)
//! to parallelize all steps during graph creation.
//!
//! The implementation uses a Compressed-Sparse-Row (CSR) data structure which
//! is tailored for fast and concurrent access to the graph topology.
//!
//! **Note**: The development is mainly driven by
//! [Neo4j](https://github.com/neo4j/neo4j) developers. However, the library is
//! __not__ an official product of Neo4j.
//!
//! # What is a graph?
//!
//! A graph consists of nodes and edges where edges connect exactly two nodes. A
//! graph can be either directed, i.e., an edge has a source and a target node
//! or undirected where there is no such distinction.
//!
//! In a directed graph, each node `u` has outgoing and incoming neighbors. An
//! outgoing neighbor of node `u` is any node `v` for which an edge `(u, v)`
//! exists. An incoming neighbor of node `u` is any node `v` for which an edge
//! `(v, u)` exists.
//!
//! In an undirected graph there is no distinction between source and target
//! node. A neighbor of node `u` is any node `v` for which either an edge `(u,
//! v)` or `(v, u)` exists.
//!
//! # How to build a graph
//!
//! The library provides a builder that can be used to construct a graph from a
//! given list of edges.
//!
//! For example, to create a directed graph that uses `usize` as node
//! identifier, one can use the builder like so:
//!
//! ```
//! use graph_builder::prelude::*;
//!
//! let graph: DirectedCsrGraph<usize> = GraphBuilder::new()
//!     .edges(vec![(0, 1), (0, 2), (1, 2), (1, 3), (2, 3)])
//!     .build();
//!
//! assert_eq!(graph.node_count(), 4);
//! assert_eq!(graph.edge_count(), 5);
//!
//! assert_eq!(graph.out_degree(1), 2);
//! assert_eq!(graph.in_degree(1), 1);
//!
//! assert_eq!(graph.out_neighbors(1).as_slice(), &[2, 3]);
//! assert_eq!(graph.in_neighbors(1).as_slice(), &[0]);
//! ```
//!
//! To build an undirected graph using `u32` as node identifer, we only need to
//! change the expected types:
//!
//! ```
//! use graph_builder::prelude::*;
//!
//! let graph: UndirectedCsrGraph<u32> = GraphBuilder::new()
//!     .csr_layout(CsrLayout::Sorted)
//!     .edges(vec![(0, 1), (0, 2), (1, 2), (1, 3), (2, 3)])
//!     .build();
//!
//! assert_eq!(graph.node_count(), 4);
//! assert_eq!(graph.edge_count(), 5);
//!
//! assert_eq!(graph.degree(1), 3);
//!
//! assert_eq!(graph.neighbors(1).as_slice(), &[0, 2, 3]);
//! ```
//!
//! Edges can have attached values to represent weighted graphs:
//!
//! ```
//! use graph_builder::prelude::*;
//!
//! let graph: UndirectedCsrGraph<u32, (), f32> = GraphBuilder::new()
//!     .csr_layout(CsrLayout::Sorted)
//!     .edges_with_values(vec![(0, 1, 0.5), (0, 2, 0.7), (1, 2, 0.25), (1, 3, 1.0), (2, 3, 0.33)])
//!     .build();
//!
//! assert_eq!(graph.node_count(), 4);
//! assert_eq!(graph.edge_count(), 5);
//!
//! assert_eq!(graph.degree(1), 3);
//!
//! assert_eq!(
//!     graph.neighbors_with_values(1).as_slice(),
//!     &[Target::new(0, 0.5), Target::new(2, 0.25), Target::new(3, 1.0)]
//! );
//! ```
//!
//! It is also possible to create a graph from a specific input format. In the
//! following example we use the `EdgeListInput` which is an input format where
//! each line of a file contains an edge of the graph.
//!
//! ```
//! use std::path::PathBuf;
//!
//! use graph_builder::prelude::*;
//!
//! let path = [env!("CARGO_MANIFEST_DIR"), "resources", "example.el"]
//!     .iter()
//!     .collect::<PathBuf>();
//!
//! let graph: DirectedCsrGraph<usize> = GraphBuilder::new()
//!     .csr_layout(CsrLayout::Sorted)
//!     .file_format(EdgeListInput::default())
//!     .path(path)
//!     .build()
//!     .expect("loading failed");
//!
//! assert_eq!(graph.node_count(), 4);
//! assert_eq!(graph.edge_count(), 5);
//!
//! assert_eq!(graph.out_degree(1), 2);
//! assert_eq!(graph.in_degree(1), 1);
//!
//! assert_eq!(graph.out_neighbors(1).as_slice(), &[2, 3]);
//! assert_eq!(graph.in_neighbors(1).as_slice(), &[0]);
//! ```
//!
//! The `EdgeListInput` format also supports weighted edges. This can be
//! controlled by a single type parameter on the graph type. Note, that the edge
//! value type needs to implement [`crate::input::ParseValue`].
//!
//! ```
//! use std::path::PathBuf;
//!
//! use graph_builder::prelude::*;
//!
//! let path = [env!("CARGO_MANIFEST_DIR"), "resources", "example.wel"]
//!     .iter()
//!     .collect::<PathBuf>();
//!
//! let graph: DirectedCsrGraph<usize, (), f32> = GraphBuilder::new()
//!     .csr_layout(CsrLayout::Sorted)
//!     .file_format(EdgeListInput::default())
//!     .path(path)
//!     .build()
//!     .expect("loading failed");
//!
//! assert_eq!(graph.node_count(), 4);
//! assert_eq!(graph.edge_count(), 5);
//!
//! assert_eq!(graph.out_degree(1), 2);
//! assert_eq!(graph.in_degree(1), 1);
//!
//! assert_eq!(
//!     graph.out_neighbors_with_values(1).as_slice(),
//!     &[Target::new(2, 0.25), Target::new(3, 1.0)]
//! );
//! assert_eq!(
//!     graph.in_neighbors_with_values(1).as_slice(),
//!     &[Target::new(0, 0.5)]
//! );
//! ```
//!
//! # Types of graphs
//!
//! The crate currently ships with two graph implementations:
//!
//! ## Compressed Sparse Row (CSR)
//!
//! [CSR](https://en.wikipedia.org/wiki/Sparse_matrix#Compressed_sparse_row_(CSR,_CRS_or_Yale_format))
//! is a data structure used for representing a sparse matrix. Since graphs can be modelled as adjacency
//! matrix and are typically very sparse, i.e., not all possible pairs of nodes are connected
//! by an edge, the CSR representation is very well suited for representing a real-world graph topology.
//!
//! In our current implementation, we use two arrays to model the edges. One array stores the adjacency
//! lists for all nodes consecutively which requires `O(edge_count)` space. The other array stores the
//! offset for each node in the first array where the corresponding adjacency list can be found which
//! requires `O(node_count)` space. The degree of a node can be inferred from the offset array.
//!
//! Our CSR implementation is immutable, i.e., once built, the topology of the graph cannot be altered as
//! it would require inserting target ids and shifting all elements to the right which is expensive and
//! invalidates all offsets coming afterwards. However, building the CSR data structure from a list of
//! edges is implement very efficiently using multi-threading.
//!
//! However, due to inlining the all adjacency lists in one `Vec`, access becomes very cache-friendly,
//! as there is a chance that the adjacency list of the next node is already cached. Also, reading the
//! graph from multiple threads is safe, as there will be never be a concurrent mutable access.
//!
//! One can use [`DirectedCsrGraph`] or [`UndirectedCsrGraph`] to build a CSR-based graph:
//!
//! ```
//! use graph_builder::prelude::*;
//!
//! let graph: DirectedCsrGraph<usize> = GraphBuilder::new()
//!     .edges(vec![(0, 1), (0, 2), (1, 2), (1, 3), (2, 3)])
//!     .build();
//!
//! assert_eq!(graph.node_count(), 4);
//! assert_eq!(graph.edge_count(), 5);
//!
//! assert_eq!(graph.out_degree(1), 2);
//! assert_eq!(graph.in_degree(1), 1);
//!
//! assert_eq!(graph.out_neighbors(1).as_slice(), &[2, 3]);
//! assert_eq!(graph.in_neighbors(1).as_slice(), &[0]);
//! ```
//!
//! ## Adjacency List (AL)
//!
//! In the Adjacency List implementation, we essentially store the graph as `Vec<Vec<ID>>`. The outer
//! `Vec` has a length of `node_count` and at each index, we store the neighbors for that particular
//! node in its own, heap-allocated `Vec`.
//!
//! The downside of that representation is that - compared to CSR - it is expected to be slower, both
//! in building it and also in reading from it, as cache misses are becoming more likely due to the
//! isolated heap allocations for individual neighbor lists.
//!
//! However, in contrast to CSR, an adjacency list is mutable, i.e., it is possible to add edges to the
//! graph even after it has been built. This makes the data structure interesting for more flexible graph
//! construction frameworks or for algorithms that need to add new edges as part of the computation.
//! Currently, adding edges is constrained by source and target node already existing in the graph.
//!
//! Internally, the individual neighbor lists for each node are protected by a `Mutex` in order to support
//! parallel read and write operations on the graph topology.
//!
//! One can use [`DirectedALGraph`] or [`UndirectedALGraph`] to build a Adjacency-List-based graph:
//!
//! ```
//! use graph_builder::prelude::*;
//!
//! let graph: DirectedALGraph<usize> = GraphBuilder::new()
//!     .edges(vec![(0, 1), (0, 2), (1, 2), (1, 3), (2, 3)])
//!     .build();
//!
//! assert_eq!(graph.node_count(), 4);
//! assert_eq!(graph.edge_count(), 5);
//!
//! assert_eq!(graph.out_degree(1), 2);
//! assert_eq!(graph.in_degree(1), 1);
//!
//! assert_eq!(graph.out_neighbors(1).as_slice(), &[2, 3]);
//! assert_eq!(graph.in_neighbors(1).as_slice(), &[0]);
//!
//! // Let's mutate the graph by adding another edge
//! graph.add_edge(1, 0);
//! assert_eq!(graph.edge_count(), 6);
//! assert_eq!(graph.out_neighbors(1).as_slice(), &[2, 3, 0]);
//! ```

pub mod builder;
mod compat;
pub mod graph;
pub mod graph_ops;
pub mod index;
pub mod input;
pub mod prelude;

pub use crate::builder::GraphBuilder;
pub use crate::graph::adj_list::DirectedALGraph;
pub use crate::graph::adj_list::UndirectedALGraph;
pub use crate::graph::csr::CsrLayout;
pub use crate::graph::csr::DirectedCsrGraph;
pub use crate::graph::csr::UndirectedCsrGraph;

use std::convert::Infallible;

use crate::graph::Target;
use crate::index::Idx;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("error while loading graph")]
    IoError {
        #[from]
        source: std::io::Error,
    },
    #[error("incompatible index type")]
    IdxError {
        #[from]
        source: std::num::TryFromIntError,
    },
    #[cfg(feature = "gdl")]
    #[cfg_attr(all(feature = "gdl", has_doc_cfg), doc(cfg(feature = "gdl")))]
    #[error("error while parsing GDL input")]
    GdlError {
        #[from]
        source: gdl::graph::GraphHandlerError,
    },
    #[error("invalid partitioning")]
    InvalidPartitioning,
    #[error("number of node values must be the same as node count")]
    InvalidNodeValues,
    #[error("invalid id size, expected {expected:?} bytes, got {actual:?} bytes")]
    InvalidIdType { expected: String, actual: String },

    #[error("node {node:?} does not exist in the graph")]
    MissingNode { node: String },
}

impl From<Infallible> for Error {
    fn from(_: Infallible) -> Self {
        unreachable!()
    }
}

/// A graph is a tuple `(N, E)`, where `N` is a set of nodes and `E` a set of
/// edges. Each edge connects exactly two nodes.
///
/// `Graph` is parameterized over the node index type `Node` which is used to
/// uniquely identify a node. An edge is a tuple of node identifiers.
pub trait Graph<NI: Idx> {
    /// Returns the number of nodes in the graph.
    fn node_count(&self) -> NI;

    /// Returns the number of edges in the graph.
    fn edge_count(&self) -> NI;
}

/// A graph that allows storing a value per node.
pub trait NodeValues<NI: Idx, NV> {
    fn node_value(&self, node: NI) -> &NV;
}

pub trait UndirectedDegrees<NI: Idx> {
    /// Returns the number of edges connected to the given node.
    fn degree(&self, node: NI) -> NI;
}

/// Returns the neighbors of a given node.
///
/// The edge `(42, 1337)` is equivalent to the edge `(1337, 42)`.
pub trait UndirectedNeighbors<NI: Idx> {
    type NeighborsIterator<'a>: Iterator<Item = &'a NI>
    where
        Self: 'a;

    /// Returns an iterator of all nodes connected to the given node.
    fn neighbors(&self, node: NI) -> Self::NeighborsIterator<'_>;
}

/// Returns the neighbors of a given node.
///
/// The edge `(42, 1337)` is equivalent to the edge `(1337, 42)`.
pub trait UndirectedNeighborsWithValues<NI: Idx, EV> {
    type NeighborsIterator<'a>: Iterator<Item = &'a Target<NI, EV>>
    where
        Self: 'a,
        EV: 'a;

    /// Returns an iterator of all nodes connected to the given node
    /// including the value of the connecting edge.
    fn neighbors_with_values(&self, node: NI) -> Self::NeighborsIterator<'_>;
}

pub trait DirectedDegrees<NI: Idx> {
    /// Returns the number of edges where the given node is a source node.
    fn out_degree(&self, node: NI) -> NI;

    /// Returns the number of edges where the given node is a target node.
    fn in_degree(&self, node: NI) -> NI;
}

/// Returns the neighbors of a given node either in outgoing or incoming direction.
///
/// An edge tuple `e = (u, v)` has a source node `u` and a target node `v`. From
/// the perspective of `u`, the edge `e` is an **outgoing** edge. From the
/// perspective of node `v`, the edge `e` is an **incoming** edge. The edges
/// `(u, v)` and `(v, u)` are not considered equivalent.
pub trait DirectedNeighbors<NI: Idx> {
    type NeighborsIterator<'a>: Iterator<Item = &'a NI>
    where
        Self: 'a;

    /// Returns an iterator of all nodes which are connected in outgoing direction
    /// to the given node, i.e., the given node is the source node of the
    /// connecting edge.
    fn out_neighbors(&self, node: NI) -> Self::NeighborsIterator<'_>;

    /// Returns an iterator of all nodes which are connected in incoming direction
    /// to the given node, i.e., the given node is the target node of the
    /// connecting edge.
    fn in_neighbors(&self, node: NI) -> Self::NeighborsIterator<'_>;
}

/// Returns the neighbors of a given node either in outgoing or incoming direction.
///
/// An edge tuple `e = (u, v)` has a source node `u` and a target node `v`. From
/// the perspective of `u`, the edge `e` is an **outgoing** edge. From the
/// perspective of node `v`, the edge `e` is an **incoming** edge. The edges
/// `(u, v)` and `(v, u)` are not considered equivale
pub trait DirectedNeighborsWithValues<NI: Idx, EV> {
    type NeighborsIterator<'a>: Iterator<Item = &'a Target<NI, EV>>
    where
        Self: 'a,
        EV: 'a;

    /// Returns an iterator of all nodes which are connected in outgoing direction
    /// to the given node, i.e., the given node is the source node of the
    /// connecting edge. For each connected node, the value of the connecting
    /// edge is also returned.
    fn out_neighbors_with_values(&self, node: NI) -> Self::NeighborsIterator<'_>;

    /// Returns an iterator of all nodes which are connected in incoming direction
    /// to the given node, i.e., the given node is the target node of the
    /// connecting edge. For each connected node, the value of the connecting
    /// edge is also returned.
    fn in_neighbors_with_values(&self, node: NI) -> Self::NeighborsIterator<'_>;
}

/// Allows adding new edges to a graph.
pub trait EdgeMutation<NI: Idx> {
    /// Adds a new edge between the given source and target node.
    ///
    /// # Errors
    ///
    /// If either the source or the target node does not exist,
    /// the method will return [`Error::MissingNode`].
    fn add_edge(&self, source: NI, target: NI) -> Result<(), Error>;

    /// Adds a new edge between the given source and target node.
    ///
    /// Does not require locking the node-local list due to `&mut self`.
    ///
    /// # Errors
    ///
    /// If either the source or the target node does not exist,
    /// the method will return [`Error::MissingNode`].
    fn add_edge_mut(&mut self, source: NI, target: NI) -> Result<(), Error>;
}

/// Allows adding new edges to a graph.
pub trait EdgeMutationWithValues<NI: Idx, EV> {
    /// Adds a new edge between the given source and target node
    /// and assigns the given value to it.
    ///
    /// # Errors
    ///
    /// If either the source or the target node does not exist,
    /// the method will return [`Error::MissingNode`].
    fn add_edge_with_value(&self, source: NI, target: NI, value: EV) -> Result<(), Error>;

    /// Adds a new edge between the given source and target node
    /// and assigns the given value to it.
    ///
    /// Does not require locking the node-local list due to `&mut self`.
    ///
    /// # Errors
    ///
    /// If either the source or the target node does not exist,
    /// the method will return [`Error::MissingNode`].
    fn add_edge_with_value_mut(&mut self, source: NI, target: NI, value: EV) -> Result<(), Error>;
}

#[repr(transparent)]
pub struct SharedMut<T>(*mut T);
unsafe impl<T: Send> Send for SharedMut<T> {}
unsafe impl<T: Sync> Sync for SharedMut<T> {}

impl<T> SharedMut<T> {
    pub fn new(ptr: *mut T) -> Self {
        SharedMut(ptr)
    }

    delegate::delegate! {
        to self.0 {
            /// # Safety
            ///
            /// Ensure that `count` does not exceed the capacity of the Vec.
            pub unsafe fn add(&self, count: usize) -> *mut T;
        }
    }
}
