//! PR #1 performance gate — on-device BGE-M3 must embed a 32-item batch
//! in under 2s on CPU so the dream-cycle backfill and live recall paths
//! stay responsive without a GPU.
//!
//! Roadmap acceptance (docs/ARCHITECTURE.md §6E-7): 32-batch CPU ≤ 2s.
//! Only meaningful when built with `--features embedding-local`; under
//! the default feature set the bench compiles to a stub that documents
//! why it did nothing, so `cargo bench` on a bare checkout doesn't fail.
//!
//! Run:
//!   cargo bench --features embedding-local --bench embedding_batch
//!
//! First run downloads ~1.1 GB of BGE-M3 weights to
//! `~/.moa/embedding-models/bge-m3/` (or `$MOA_EMBEDDING_CACHE`).

#[cfg(not(feature = "embedding-local"))]
fn main() {
    eprintln!(
        "embedding_batch bench skipped — rebuild with `--features embedding-local` \
         to exercise the real BGE-M3 path"
    );
}

#[cfg(feature = "embedding-local")]
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};

#[cfg(feature = "embedding-local")]
use std::hint::black_box;

#[cfg(feature = "embedding-local")]
use zeroclaw::memory::embedding::local_fastembed::{create, DEFAULT_DIM, DEFAULT_MODEL};

/// Korean-leaning sentence pool. BGE-M3 is multilingual so a mixed
/// Korean/English input set is more representative of production
/// traffic than pure English. We intentionally keep sentence lengths
/// varied (short titles, mid-length descriptions, paragraph-ish chunks)
/// so the tokeniser's padding behaviour is exercised.
#[cfg(feature = "embedding-local")]
const SENTENCES: &[&str] = &[
    "나는 변호사다.",
    "주택임대차보호법상 대항력 발생 시점을 확인한다.",
    "hello world",
    "This morning I reviewed the purchase and sale agreement.",
    "계약서 제3조에 명시된 해제권 행사 기한을 체크할 것.",
    "The plaintiff's counsel filed a motion to compel discovery.",
    "보증금 반환 청구의 소 요건사실을 정리한다.",
    "I need to email opposing counsel before 5pm today.",
    "공시송달 요건을 갖추지 못한 경우 재판은 무효가 된다.",
    "Draft a memo summarising the tenant's options.",
    "임차권등기명령 신청 서류 일체를 준비한다.",
    "Review the deposition transcript for inconsistencies.",
    "민법 제548조 해제의 효과에 관한 판례 요약.",
    "Research the statute of limitations for breach of contract.",
    "근저당권 말소등기 청구 가능 여부 검토.",
    "Prepare a client letter summarising next steps.",
    "임대인이 계약 갱신 거절 사유를 서면으로 통지해야 한다.",
    "Review invoices for the past quarter.",
    "전세권 설정 계약서 초안을 의뢰인에게 전달.",
    "Schedule a meeting with the expert witness next week.",
    "채권자 대위권 행사의 요건과 효과 분석.",
    "Update the case management system with today's filings.",
    "가처분 신청서 제출 전 보전의 필요성 재검토.",
    "Read the latest appellate court opinion on this issue.",
    "원상회복 의무의 범위에 관한 대법원 판례 확인.",
    "Finalize the engagement letter for the new client.",
    "채무불이행에 따른 손해배상 범위 검토.",
    "Check the court's online docket for updates.",
    "소송비용 확정 결정 신청의 절차를 숙지한다.",
    "Review the draft contract with the client tomorrow.",
    "부동산 명도 집행 절차의 실무 포인트 정리.",
    "Plan the week's billable-hour targets.",
];

#[cfg(feature = "embedding-local")]
fn bench_cpu_32_batch(c: &mut Criterion) {
    assert_eq!(
        SENTENCES.len(),
        32,
        "bench input pool must be exactly 32 items"
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    // Build the provider once — model load + cache download (if needed)
    // must not be measured. If initialisation fails, bail loudly rather
    // than measuring a stub.
    let provider = create(DEFAULT_MODEL, DEFAULT_DIM);
    assert_eq!(
        provider.name(),
        "local_fastembed",
        "expected real provider; rebuild with `--features embedding-local`"
    );

    // Warm-up: first call amortises JIT + tokenizer init. We do it
    // outside the timed loop so sample 1 isn't artificially slow.
    runtime
        .block_on(provider.embed(SENTENCES))
        .expect("warm-up embed failed — check ~/.moa/embedding-models cache and network");

    let mut group = c.benchmark_group("embedding_batch");
    group.throughput(Throughput::Elements(32));
    // BGE-M3 on CPU is slow (hundreds of ms per batch). Keep the sample
    // size small and the measurement time generous so criterion doesn't
    // complain or inflate CI wall-clock.
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(30));
    group.bench_function("bge_m3_cpu_32", |b| {
        b.iter_batched(
            || SENTENCES,
            |texts| {
                let out = runtime
                    .block_on(provider.embed(texts))
                    .expect("embed failed mid-bench");
                black_box(out);
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

#[cfg(feature = "embedding-local")]
criterion_group!(benches, bench_cpu_32_batch);

#[cfg(feature = "embedding-local")]
criterion_main!(benches);
