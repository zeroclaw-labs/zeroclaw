use atomic::Atomic;
use log::info;
use std::{convert::TryFrom, fs::File, marker::PhantomData, path::Path, sync::Arc};

use crate::index::Idx;

use parking_lot::Mutex;
use rayon::prelude::*;
use std::sync::atomic::Ordering::AcqRel;

use crate::{input::Direction, Error};

use super::{InputCapabilities, InputPath, ParseValue};

/// Reads a graph from a file that contains an edge per line.
///
/// An edge is represented by a source node id and a target node id. The two
/// node ids must be separated by a 1-byte character (e.g. whitespace or tab).
///
/// The node count of the resulting graph is the highest node id within the file
/// plus one. The edge count will be twice the number of lines in the file.
///
/// # Example
///
/// ```ignore
/// > cat my_graph.edgelist
/// 0 1
/// 0 2
/// 1 3
/// 2 0
/// ```
pub struct EdgeListInput<NI: Idx, EV = ()> {
    _idx: PhantomData<(NI, EV)>,
}

impl<NI: Idx, EV> Default for EdgeListInput<NI, EV> {
    fn default() -> Self {
        Self { _idx: PhantomData }
    }
}

impl<NI: Idx, EV> InputCapabilities<NI> for EdgeListInput<NI, EV> {
    type GraphInput = EdgeList<NI, EV>;
}

#[allow(clippy::len_without_is_empty)]
pub trait Edges {
    type NI: Idx;
    type EV;

    type EdgeIter<'a>: ParallelIterator<Item = (Self::NI, Self::NI, Self::EV)>
    where
        Self: 'a;

    fn edges(&self) -> Self::EdgeIter<'_>;

    fn max_node_id(&self) -> Self::NI {
        default_max_node_id(self)
    }

    fn degrees(&self, node_count: Self::NI, direction: Direction) -> Vec<Atomic<Self::NI>> {
        let mut degrees = Vec::with_capacity(node_count.index());
        degrees.resize_with(node_count.index(), || Atomic::new(Self::NI::zero()));

        if matches!(direction, Direction::Outgoing | Direction::Undirected) {
            self.edges().for_each(|(s, _, _)| {
                Self::NI::get_and_increment(&degrees[s.index()], AcqRel);
            });
        }

        if matches!(direction, Direction::Incoming | Direction::Undirected) {
            self.edges().for_each(|(_, t, _)| {
                Self::NI::get_and_increment(&degrees[t.index()], AcqRel);
            });
        }

        degrees
    }

    #[cfg(test)]
    fn len(&self) -> usize;
}

fn default_max_node_id<E: Edges + ?Sized>(edges: &E) -> E::NI {
    edges
        .edges()
        .into_par_iter()
        .map(|(s, t, _)| E::NI::max(s, t))
        .reduce(E::NI::zero, E::NI::max)
}

#[derive(Debug)]
pub struct EdgeList<NI: Idx, EV> {
    list: Box<[(NI, NI, EV)]>,
    max_node_id: Option<NI>,
}

impl<NI: Idx, EV: Sync> EdgeList<NI, EV> {
    pub fn new(edges: Vec<(NI, NI, EV)>) -> Self {
        Self {
            list: edges.into_boxed_slice(),
            max_node_id: None,
        }
    }

    pub fn with_max_node_id(edges: Vec<(NI, NI, EV)>, max_node_id: NI) -> Self {
        Self {
            list: edges.into_boxed_slice(),
            max_node_id: Some(max_node_id),
        }
    }
}

impl<NI: Idx, EV: Copy + Send + Sync> Edges for EdgeList<NI, EV> {
    type NI = NI;

    type EV = EV;

    type EdgeIter<'a>
        = rayon::iter::Copied<rayon::slice::Iter<'a, (Self::NI, Self::NI, Self::EV)>>
    where
        Self: 'a;

    fn edges(&self) -> Self::EdgeIter<'_> {
        self.list.par_iter().copied()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.list.len()
    }

    fn max_node_id(&self) -> Self::NI {
        match self.max_node_id {
            Some(id) => id,
            None => default_max_node_id(self),
        }
    }
}

pub(crate) struct EdgeIterator<NI: Idx, I: IntoIterator<Item = (NI, NI)>>(pub I);

impl<NI, I> From<EdgeIterator<NI, I>> for EdgeList<NI, ()>
where
    NI: Idx,
    I: IntoIterator<Item = (NI, NI)>,
{
    fn from(iter: EdgeIterator<NI, I>) -> Self {
        EdgeList::new(iter.0.into_iter().map(|(s, t)| (s, t, ())).collect())
    }
}

pub(crate) struct EdgeWithValueIterator<NI: Idx, EV, I: IntoIterator<Item = (NI, NI, EV)>>(pub I);

impl<NI, EV, I> From<EdgeWithValueIterator<NI, EV, I>> for EdgeList<NI, EV>
where
    NI: Idx,
    EV: Sync,
    I: IntoIterator<Item = (NI, NI, EV)>,
{
    fn from(iter: EdgeWithValueIterator<NI, EV, I>) -> Self {
        EdgeList::new(iter.0.into_iter().collect())
    }
}

impl<NI, P, EV> TryFrom<InputPath<P>> for EdgeList<NI, EV>
where
    P: AsRef<Path>,
    NI: Idx,
    EV: ParseValue + std::fmt::Debug + Send + Sync,
{
    type Error = Error;

    fn try_from(path: InputPath<P>) -> Result<Self, Self::Error> {
        let file = File::open(path.0.as_ref())?;
        let mmap = unsafe { memmap2::MmapOptions::new().populate().map(&file)? };
        EdgeList::try_from(mmap.as_ref())
    }
}

impl<NI, EV> TryFrom<&[u8]> for EdgeList<NI, EV>
where
    NI: Idx,
    EV: ParseValue + std::fmt::Debug + Send + Sync,
{
    type Error = Error;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let start = std::time::Instant::now();

        let page_size = page_size::get();
        let cpu_count = num_cpus::get_physical();
        let chunk_size =
            (usize::max(1, bytes.len() / cpu_count) + (page_size - 1)) & !(page_size - 1);

        info!(
            "page_size = {}, cpu_count = {}, chunk_size = {}",
            page_size, cpu_count, chunk_size
        );

        let all_edges = Arc::new(Mutex::new(Vec::new()));

        let new_line_bytes = new_line_bytes(bytes);

        std::thread::scope(|s| {
            for start in (0..bytes.len()).step_by(chunk_size) {
                let all_edges = Arc::clone(&all_edges);
                s.spawn(move || {
                    let mut end = usize::min(start + chunk_size, bytes.len());
                    while end <= bytes.len() && bytes[end - 1] != b'\n' {
                        end += 1;
                    }

                    let mut start = start;
                    if start != 0 {
                        while bytes[start - 1] != b'\n' {
                            start += 1;
                        }
                    }

                    let mut edges = Vec::new();
                    let mut chunk = &bytes[start..end];
                    while !chunk.is_empty() {
                        let (source, source_bytes) = NI::parse(chunk);
                        chunk = &chunk[source_bytes + 1..];

                        let (target, target_bytes) = NI::parse(chunk);
                        chunk = &chunk[target_bytes..];

                        let value = match chunk.strip_prefix(b" ") {
                            Some(value_chunk) => {
                                let (value, value_bytes) = EV::parse(value_chunk);
                                chunk = &value_chunk[value_bytes + new_line_bytes..];
                                value
                            }
                            None => {
                                chunk = &chunk[new_line_bytes..];
                                // if the input does not have a value, the default for EV is used
                                EV::parse(&[]).0
                            }
                        };

                        edges.push((source, target, value));
                    }

                    let mut all_edges = all_edges.lock();
                    all_edges.append(&mut edges);
                });
            }
        });

        let edges = Arc::try_unwrap(all_edges).unwrap().into_inner();

        let elapsed = start.elapsed().as_millis() as f64 / 1000_f64;

        info!(
            "Read {} edges in {:.2}s ({:.2} MB/s)",
            edges.len(),
            elapsed,
            ((bytes.len() as f64) / elapsed) / (1024.0 * 1024.0)
        );

        Ok(EdgeList::new(edges))
    }
}

// Returns the OS-dependent number of bytes for newline:
//
// `1` for Linux/macOS style (b'\n')
// '2' for Windows style (b'\r\n')
fn new_line_bytes(bytes: &[u8]) -> usize {
    1 + bytes
        .iter()
        .position(|b| *b == b'\n')
        .and_then(|idx| idx.checked_sub(1))
        .and_then(|idx| bytes.get(idx).copied())
        .map_or(0, |b| (b == b'\r') as usize)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::input::InputPath;

    use super::*;

    #[test]
    fn edge_list_from_linux_file() {
        let path = [env!("CARGO_MANIFEST_DIR"), "resources", "test.el"]
            .iter()
            .collect::<PathBuf>();

        let expected: Vec<(usize, usize, ())> = vec![
            (0, 1, ()),
            (0, 2, ()),
            (1, 2, ()),
            (1, 3, ()),
            (2, 4, ()),
            (3, 4, ()),
        ];

        let edge_list = EdgeList::<usize, ()>::try_from(InputPath(path.as_path())).unwrap();

        assert_eq!(4, edge_list.max_node_id());

        let edge_list = edge_list.list.into_vec();

        assert_eq!(expected, edge_list)
    }

    #[test]
    fn edge_list_with_values_from_file() {
        let path = [env!("CARGO_MANIFEST_DIR"), "resources", "test.wel"]
            .iter()
            .collect::<PathBuf>();

        let expected: Vec<(usize, usize, f32)> = vec![
            (0, 1, 0.1),
            (0, 2, 0.2),
            (1, 2, 0.3),
            (1, 3, 0.4),
            (2, 4, 0.5),
            (3, 4, 0.6),
        ];

        let edge_list = EdgeList::<usize, f32>::try_from(InputPath(path.as_path())).unwrap();

        assert_eq!(4, edge_list.max_node_id());

        let edge_list = edge_list.list.into_vec();

        assert_eq!(expected, edge_list)
    }

    #[test]
    fn edge_list_from_windows_file() {
        let path = [env!("CARGO_MANIFEST_DIR"), "resources", "windows.el"]
            .iter()
            .collect::<PathBuf>();

        println!("{path:?}");

        let edge_list = EdgeList::<usize, ()>::try_from(InputPath(path.as_path())).unwrap();

        assert_eq!(3, edge_list.max_node_id());
    }
}
