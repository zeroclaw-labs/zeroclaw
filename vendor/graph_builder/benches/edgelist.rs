use criterion::{black_box, criterion_group, criterion_main, Criterion, SamplingMode};
use graph_builder::prelude::*;

mod common;
use common::*;

fn max_node_id(c: &mut Criterion) {
    let mut group = c.benchmark_group("max_node_id");
    group.sampling_mode(SamplingMode::Flat);

    group.bench_function(SMALL.name, |b| bench_max_node_id(b, SMALL));
    group.bench_function(MEDIUM.name, |b| bench_max_node_id(b, MEDIUM));
    group.bench_function(LARGE.name, |b| bench_max_node_id(b, LARGE));

    group.finish();
}

fn bench_max_node_id(
    b: &mut criterion::Bencher,
    Input {
        name: _,
        node_count,
        edge_count,
    }: Input,
) {
    let input: Vec<(usize, usize, ())> = uniform_edge_list(node_count, edge_count, |_, _| ());
    b.iter_batched(
        || EdgeList::new(input.clone()),
        |edge_list| black_box(edge_list.max_node_id()),
        criterion::BatchSize::SmallInput,
    )
}

fn degrees(c: &mut Criterion) {
    let mut group = c.benchmark_group("degrees");
    group.sampling_mode(SamplingMode::Flat);

    for direction in [
        Direction::Outgoing,
        Direction::Incoming,
        Direction::Undirected,
    ] {
        group.bench_function(format!("{}_{direction:?}", SMALL.name), |b| {
            bench_degrees(b, SMALL, direction)
        });
        group.bench_function(format!("{}_{direction:?}", MEDIUM.name), |b| {
            bench_degrees(b, MEDIUM, direction)
        });
        group.bench_function(format!("{}_{direction:?}", LARGE.name), |b| {
            bench_degrees(b, LARGE, direction)
        });
    }

    group.finish();
}

fn bench_degrees(
    b: &mut criterion::Bencher,
    Input {
        name: _,
        node_count,
        edge_count,
    }: Input,
    direction: Direction,
) {
    let edges: Vec<(usize, usize, ())> = uniform_edge_list(node_count, edge_count, |_, _| ());
    b.iter_batched(
        || EdgeList::new(edges.clone()),
        |edge_list| black_box(edge_list.degrees(node_count, direction)),
        criterion::BatchSize::SmallInput,
    )
}

criterion_group!(benches, max_node_id, degrees);
criterion_main!(benches);
