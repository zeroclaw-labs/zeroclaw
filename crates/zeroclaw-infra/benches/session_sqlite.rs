//! Benchmarks for the SQLite session transcript write paths: the per-turn
//! append, the unchanged-transcript rewrite, a history trim and a compaction.

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_infra::session_backend::SessionBackend;
use zeroclaw_infra::session_sqlite::SqliteSessionBackend;

const KEY: &str = "bench-session";
const SIZES: [usize; 3] = [20, 200, 1000];
/// Filler repeats per message: about 230 bytes for a chat turn, about 20 KB
/// for a tool result close to the configured size cap.
const SMALL_FILLER: usize = 8;
const LARGE_FILLER: usize = 740;
const LARGE_SIZES: [usize; 2] = [200, 1000];

fn transcript(len: usize, filler: usize) -> Vec<ChatMessage> {
    (0..len)
        .map(|i| {
            let body = format!(
                "message {i}: {}",
                "lorem ipsum dolor sit amet ".repeat(filler)
            );
            if i % 2 == 0 {
                ChatMessage::user(body)
            } else {
                ChatMessage::assistant(body)
            }
        })
        .collect()
}

fn seeded(
    len: usize,
    filler: usize,
) -> (tempfile::TempDir, SqliteSessionBackend, Vec<ChatMessage>) {
    let tmp = tempfile::TempDir::new().unwrap();
    let backend = SqliteSessionBackend::new(tmp.path()).unwrap();
    let messages = transcript(len, filler);
    backend.rewrite_messages(KEY, &messages).unwrap();
    (tmp, backend, messages)
}

fn bench_session_writes(c: &mut Criterion) {
    bench_writes(c, "session_sqlite", &SIZES, SMALL_FILLER);
}

fn bench_session_writes_large_messages(c: &mut Criterion) {
    bench_writes(c, "session_sqlite_large", &LARGE_SIZES, LARGE_FILLER);
}

fn bench_writes(c: &mut Criterion, name: &str, sizes: &[usize], filler: usize) {
    let mut group = c.benchmark_group(name);
    for &len in sizes {
        group.bench_with_input(BenchmarkId::new("append_turn", len), &len, |b, &len| {
            b.iter_batched(
                || seeded(len, filler),
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
                let (_tmp, backend, messages) = seeded(len, filler);
                b.iter(|| backend.rewrite_messages(KEY, black_box(&messages)).unwrap());
            },
        );

        group.bench_with_input(
            BenchmarkId::new("rewrite_plus_turn", len),
            &len,
            |b, &len| {
                b.iter_batched(
                    || {
                        let (tmp, backend, mut messages) = seeded(len, filler);
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
                        let (tmp, backend, messages) = seeded(len, filler);
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
                        let (tmp, backend, messages) = seeded(len, filler);
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
                let (_tmp, backend, _) = seeded(len, filler);
                b.iter(|| black_box(backend.load_with_timestamps(KEY)));
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_session_writes,
    bench_session_writes_large_messages
);
criterion_main!(benches);
