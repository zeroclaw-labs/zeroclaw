use log::info;
use std::{fs::File, marker::PhantomData, path::Path};

use crate::prelude::*;
use rayon::prelude::*;

pub struct Graph500Input<NI> {
    _phantom: PhantomData<NI>,
}

impl<NI> Default for Graph500Input<NI> {
    fn default() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl<NI: Idx> InputCapabilities<NI> for Graph500Input<NI> {
    type GraphInput = Graph500<NI>;
}

pub struct Graph500<NI: Idx>(pub EdgeList<NI, ()>);

impl<NI: Idx> Edges for Graph500<NI> {
    type NI = NI;

    type EV = ();

    type EdgeIter<'a>
        = rayon::iter::Copied<rayon::slice::Iter<'a, (Self::NI, Self::NI, Self::EV)>>
    where
        Self: 'a;

    fn edges(&self) -> Self::EdgeIter<'_> {
        self.0.edges()
    }

    fn max_node_id(&self) -> Self::NI {
        self.0.max_node_id()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.len()
    }
}

impl<NI, P> TryFrom<InputPath<P>> for Graph500<NI>
where
    P: AsRef<Path>,
    NI: Idx,
{
    type Error = Error;

    fn try_from(path: InputPath<P>) -> Result<Self, Self::Error> {
        let file = File::open(path.0.as_ref())?;
        let mmap = unsafe { memmap2::MmapOptions::new().populate().map(&file)? };
        Graph500::try_from(mmap.as_ref())
    }
}

impl<NI> TryFrom<&[u8]> for Graph500<NI>
where
    NI: Idx,
{
    type Error = Error;

    fn try_from(map: &[u8]) -> Result<Self, Self::Error> {
        let start = std::time::Instant::now();

        let file_size = map.len();
        let edge_count = map.len() / std::mem::size_of::<PackedEdge>();
        let node_count = edge_count / 16;

        let map = map.as_ptr();
        assert_eq!(map as usize % std::mem::align_of::<PackedEdge>(), 0);

        let edges = unsafe { std::slice::from_raw_parts(map as *const PackedEdge, edge_count) };

        let mut all_edges = Vec::with_capacity(edge_count);

        edges
            .par_iter()
            .map(|edge| {
                let source =
                    usize::try_from(edge.source()).expect("Could not read source id as usize");
                let target =
                    usize::try_from(edge.target()).expect("Could not read target id as usize");

                (NI::new(source), NI::new(target), ())
            })
            .collect_into_vec(&mut all_edges);

        let edges = EdgeList::with_max_node_id(all_edges, NI::new(node_count - 1));

        let elapsed = start.elapsed().as_millis() as f64 / 1000_f64;

        info!(
            "Read {} edges in {:.2}s ({:.2} MB/s)",
            edge_count,
            elapsed,
            ((file_size as f64) / elapsed) / (1024.0 * 1024.0)
        );

        Ok(Self(edges))
    }
}

// see https://github.com/graph500/graph500/blob/f89d643ce4aaae9a823d310c6ab2dd10e3d2982c/generator/graph_generator.h#L29-L33
#[derive(Default, Copy, Clone, Debug)]
#[repr(C)]
struct PackedEdge {
    v0_low: u32,
    v1_low: u32,
    high: u32,
}

impl PackedEdge {
    fn source(&self) -> u64 {
        self.v0_low as u64 | (self.high as u64 & 0xFFFF) << 32
    }

    fn target(&self) -> u64 {
        self.v1_low as u64 | (self.high as u64 >> 16) << 32
    }
}
