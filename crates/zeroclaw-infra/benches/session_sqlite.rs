//! Benchmarks for the SQLite session transcript write paths: the per-turn
//! append, the unchanged-transcript rewrite, a history trim and a compaction.

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_infra::session_backend::SessionBackend;
use zeroclaw_infra::session_sqlite::SqliteSessionBackend;

const KEY: &str = "bench-session";
const SIZES: [usize; 3] = [20, 200, 1000];

fn transcript(len: usize) -> Vec<ChatMessage> {
    (0..len)
        .map(|i| {
            let body = format!("message {i}: {}", "lorem ipsum dolor sit amet ".repeat(8));
            if i % 2 == 0 {
                ChatMessage::user(body)
            } else {
                ChatMessage::assistant(body)
            }
        })
        .collect()
}

fn seeded(len: usize) -> (tempfile::TempDir, SqliteSessionBackend, Vec<ChatMessage>) {
    let tmp = tempfile::TempDir::new().unwrap();
    let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
    let messages = transcript(len);
    backend.rewrite_messages(KEY, &messages).unwrap();
    (tmp, backend, messages)
}

fn bench_session_writes(c: &mut Criterion) {
    let mut group = c.benchmark_group("session_sqlite");
    for len in SIZES {
        group.bench_with_input(BenchmarkId::new("append_turn", len), &len, |b, &len| {
            b.iter_batched(
                || seeded(len),
                |(tmp, backend, _)| {
                    backend
                        .append(KEY, &ChatMessage::user("next turn"))
                        .unwrap();
                    (tmp, backend)
                },
                BatchSize::PerIteration,
            );
        });

        group.bench_with_input(
            BenchmarkId::new("rewrite_unchanged", len),
            &len,
            |b, &len| {
                let (_tmp, backend, messages) = seeded(len);
                b.iter(|| backend.rewrite_messages(KEY, black_box(&messages)).unwrap());
            },
        );

        group.bench_with_input(
            BenchmarkId::new("rewrite_plus_turn", len),
            &len,
            |b, &len| {
                b.iter_batched(
                    || {
                        let (tmp, backend, mut messages) = seeded(len);
                        messages.push(ChatMessage::user("next turn"));
                        (tmp, backend, messages)
                    },
                    |(tmp, backend, messages)| {
                        backend.rewrite_messages(KEY, &messages).unwrap();
                        (tmp, backend)
                    },
                    BatchSize::PerIteration,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("rewrite_trim_half", len),
            &len,
            |b, &len| {
                b.iter_batched(
                    || {
                        let (tmp, backend, messages) = seeded(len);
                        let trimmed = messages[len / 2..].to_vec();
                        (tmp, backend, trimmed)
                    },
                    |(tmp, backend, trimmed)| {
                        backend.rewrite_messages(KEY, &trimmed).unwrap();
                        (tmp, backend)
                    },
                    BatchSize::PerIteration,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("rewrite_compaction", len),
            &len,
            |b, &len| {
                b.iter_batched(
                    || {
                        let (tmp, backend, messages) = seeded(len);
                        let mut compacted = vec![ChatMessage::system("summary of earlier turns")];
                        compacted.extend_from_slice(&messages[len - len / 10..]);
                        (tmp, backend, compacted)
                    },
                    |(tmp, backend, compacted)| {
                        backend.rewrite_messages(KEY, &compacted).unwrap();
                        (tmp, backend)
                    },
                    BatchSize::PerIteration,
                );
            },
        );

        group.bench_with_input(
            BenchmarkId::new("load_with_timestamps", len),
            &len,
            |b, &len| {
                let (_tmp, backend, _) = seeded(len);
                b.iter(|| black_box(backend.load_with_timestamps(KEY)));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_session_writes);
criterion_main!(benches);
