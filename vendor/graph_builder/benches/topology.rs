use criterion::measurement::WallTime;
use criterion::*;
use graph_builder::graph::csr::Csr;
use graph_builder::prelude::*;

use graph_builder::graph::adj_list::{AdjacencyList, DirectedALGraph};

mod common;
use common::*;

fn csr(c: &mut Criterion) {
    let mut group = c.benchmark_group("csr_from_edge_list");
    group.sampling_mode(SamplingMode::Flat);
    from_edge_list::<Csr<usize, usize, ()>>(group);

    let mut group = c.benchmark_group("csr_all_targets");
    group.sampling_mode(SamplingMode::Flat);
    all_targets::<DirectedCsrGraph<usize>>(group);
}

fn adjacency_list(c: &mut Criterion) {
    let mut group = c.benchmark_group("adjacency_list_from_edge_list");
    group.sampling_mode(SamplingMode::Flat);
    from_edge_list::<AdjacencyList<usize, ()>>(group);

    let mut group = c.benchmark_group("adjacency_list_all_targets");
    group.sampling_mode(SamplingMode::Flat);
    all_targets::<DirectedALGraph<usize>>(group);
}

fn from_edge_list<T>(mut group: BenchmarkGroup<WallTime>)
where
    T: for<'a> From<(&'a EdgeList<usize, ()>, usize, Direction, CsrLayout)>,
{
    for direction in [
        Direction::Outgoing,
        Direction::Incoming,
        Direction::Undirected,
    ] {
        for csr_layout in [
            CsrLayout::Unsorted,
            CsrLayout::Sorted,
            CsrLayout::Deduplicated,
        ] {
            group.bench_function(
                format!("{}_{direction:?}_{csr_layout:?}", SMALL.name),
                |b| bench_from_edge_list::<T>(b, SMALL, direction, csr_layout),
            );
            group.bench_function(
                format!("{}_{direction:?}_{csr_layout:?}", MEDIUM.name),
                |b| bench_from_edge_list::<T>(b, MEDIUM, direction, csr_layout),
            );
            group.bench_function(
                format!("{}_{direction:?}_{csr_layout:?}", LARGE.name),
                |b| bench_from_edge_list::<T>(b, LARGE, direction, csr_layout),
            );
        }
    }

    group.finish();
}

fn bench_from_edge_list<T>(
    b: &mut Bencher,
    Input {
        name: _,
        node_count,
        edge_count,
    }: Input,
    direction: Direction,
    csr_layout: CsrLayout,
) where
    T: for<'a> From<(&'a EdgeList<usize, ()>, usize, Direction, CsrLayout)>,
{
    let edges: Vec<(usize, usize, ())> = uniform_edge_list(node_count, edge_count, |_, _| ());
    b.iter_batched(
        || EdgeList::new(edges.clone()),
        |edge_list| black_box(T::from((&edge_list, node_count, direction, csr_layout))),
        criterion::BatchSize::SmallInput,
    )
}

fn all_targets<G>(mut group: BenchmarkGroup<WallTime>)
where
    G: From<(EdgeList<usize, ()>, CsrLayout)> + DirectedNeighbors<usize>,
{
    for direction in [Direction::Outgoing, Direction::Incoming] {
        for csr_layout in [
            CsrLayout::Unsorted,
            CsrLayout::Sorted,
            CsrLayout::Deduplicated,
        ] {
            group.bench_function(
                format!("{}_{direction:?}_{csr_layout:?}", SMALL.name),
                |b| bench_all_targets::<G>(b, SMALL, direction, csr_layout),
            );
            group.bench_function(
                format!("{}_{direction:?}_{csr_layout:?}", MEDIUM.name),
                |b| bench_all_targets::<G>(b, MEDIUM, direction, csr_layout),
            );
            group.bench_function(
                format!("{}_{direction:?}_{csr_layout:?}", LARGE.name),
                |b| bench_all_targets::<G>(b, LARGE, direction, csr_layout),
            );
        }
    }

    group.finish();
}

fn bench_all_targets<G>(
    b: &mut Bencher,
    Input {
        name: _,
        node_count,
        edge_count,
    }: Input,
    direction: Direction,
    csr_layout: CsrLayout,
) where
    G: From<(EdgeList<usize, ()>, CsrLayout)> + DirectedNeighbors<usize>,
{
    let edges: Vec<(usize, usize, ())> = uniform_edge_list(node_count, edge_count, |_, _| ());

    b.iter_batched(
        || {
            let edges = EdgeList::new(edges.clone());
            G::from((edges, csr_layout))
        },
        |graph| {
            for node in 0..node_count {
                let neighbors = match direction {
                    Direction::Outgoing => graph.out_neighbors(node),
                    Direction::Incoming => graph.in_neighbors(node),
                    Direction::Undirected => unreachable!(),
                };
                black_box(neighbors.collect::<Vec<_>>());
            }
        },
        criterion::BatchSize::SmallInput,
    )
}

criterion_group!(benches, csr, adjacency_list);
criterion_main!(benches);
