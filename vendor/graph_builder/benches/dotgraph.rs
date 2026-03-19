use criterion::{black_box, criterion_group, criterion_main, Criterion, SamplingMode};
use graph_builder::prelude::*;

use graph_builder::input::dotgraph::{LabelStats, NodeLabelIndex};
use rand::Rng;

mod common;
use common::*;

fn label_stats(c: &mut Criterion) {
    let mut group = c.benchmark_group("label_stats");

    group.sampling_mode(SamplingMode::Flat);

    group.bench_function(SMALL.name, |b| bench_label_stats(b, SMALL));
    group.bench_function(MEDIUM.name, |b| bench_label_stats(b, MEDIUM));
    group.bench_function(LARGE.name, |b| bench_label_stats(b, LARGE));

    group.finish();
}

fn bench_label_stats(b: &mut criterion::Bencher, Input { node_count, .. }: Input) {
    let labels = node_values(node_count, |_node, rng| rng.gen_range(0..42));
    let graph: UndirectedCsrGraph<usize, usize> = GraphBuilder::new()
        .edges([(0, node_count - 1)])
        .node_values(labels.clone())
        .build();

    b.iter(|| black_box(LabelStats::from_graph(&graph)))
}

fn node_label_index(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_label_index");
    group.sampling_mode(SamplingMode::Flat);

    group.bench_function(SMALL.name, |b| bench_node_label_index(b, SMALL));
    group.bench_function(MEDIUM.name, |b| bench_node_label_index(b, MEDIUM));
    group.bench_function(LARGE.name, |b| bench_node_label_index(b, LARGE));

    group.finish();
}

fn bench_node_label_index(b: &mut criterion::Bencher, Input { node_count, .. }: Input) {
    let labels = node_values(node_count, |_node, rng| rng.gen_range(0..42));
    let graph: UndirectedCsrGraph<usize, usize> = GraphBuilder::new()
        .edges([(0, node_count - 1)])
        .node_values(labels.clone())
        .build();
    let stats = LabelStats::from_graph(&graph);

    b.iter(|| {
        black_box(NodeLabelIndex::from_stats(node_count, &stats, |node| {
            labels[node]
        }))
    })
}

criterion_group!(benches, label_stats, node_label_index);
criterion_main!(benches);
