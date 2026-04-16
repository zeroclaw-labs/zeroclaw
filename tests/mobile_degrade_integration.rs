//! PR #4 mobile-build integration test.
//!
//! The mobile-posture unit test in `src/memory/sqlite.rs` locks the
//! per-recall contract: retrieval keeps working when neither the BGE-M3
//! embedder nor the BGE reranker are compiled into the binary. This
//! integration test runs the same contract through a full end-to-end
//! seed → recall cycle on a SyncedMemory wrapper, which is what the
//! mobile app actually hands the agent loop.
//!
//! Specifically we check, against the default feature set (no
//! `embedding-local`, no reranker attachment, no reranker feature
//! flag), that:
//!
//! 1. A fresh SyncedMemory constructed on top of SqliteMemory seeds and
//!    recalls all three eval corpus domains.
//! 2. `recall_with_variations(&[], top_k, None)` routes through to
//!    `recall()` (the mobile fast path) without panicking.
//! 3. `recall_with_variations(&[rewrite1, rewrite2], top_k, None)` —
//!    the agent loop's query-expansion path — still returns hits via
//!    RRF even though no reranker can reorder them.
//!
//! Device-level tests on actual iOS/Android hardware still need to run
//! through the Tauri harness (`clients/tauri/src-tauri/` mobile build),
//! but this catches regressions in the shared Rust retrieval code path
//! before they hit a device-lab.

use std::sync::Arc;

use parking_lot::Mutex;
use tempfile::TempDir;

use zeroclaw::memory::sqlite::SqliteMemory;
use zeroclaw::memory::sync::SyncEngine;
use zeroclaw::memory::synced::SyncedMemory;
use zeroclaw::memory::traits::{Memory, MemoryCategory};

fn mobile_build_memory(tmp: &TempDir) -> SyncedMemory {
    let inner = Arc::new(
        SqliteMemory::new(tmp.path()).expect("open SqliteMemory in mobile-like temp workspace"),
    );
    // SyncEngine disabled — mobile installs default to sync-off until
    // the user pairs a second device. This also keeps the test hermetic
    // (no extraneous sync_state.db writes).
    let engine = SyncEngine::new(tmp.path(), false).expect("SyncEngine::new");
    let engine_arc = Arc::new(Mutex::new(engine));
    inner.attach_sync(engine_arc.clone());
    SyncedMemory::new(inner, engine_arc)
}

#[tokio::test]
async fn mobile_build_recall_returns_hits_across_all_three_domains() {
    let tmp = TempDir::new().expect("tempdir");
    let mem = mobile_build_memory(&tmp);

    for (k, v) in [
        ("law_1", "주택임대차보호법 제3조는 대항력의 발생 요건을 규정한다."),
        ("law_2", "민법 제548조는 계약 해제의 효과를 정한다."),
        ("ko_1", "사용자는 부동산 임대차 분쟁 전담 변호사다."),
        ("ko_2", "주말 운동 루틴은 테니스와 수영이다."),
        ("en_1", "Weekly retrospective runs on Fridays at 10:00 KST."),
        ("en_2", "Code reviews require at least one CODEOWNERS approval."),
    ] {
        mem.store(k, v, MemoryCategory::Core, None)
            .await
            .expect("seed store");
    }

    for (query, expected) in [
        ("대항력 발생", "law_1"),
        ("임대차 전담", "ko_1"),
        ("CODEOWNERS approval", "en_2"),
    ] {
        let hits = mem.recall(query, 5, None).await.expect("recall");
        assert!(
            hits.iter().any(|h| h.key == expected),
            "mobile build recall for `{query}` should surface `{expected}`, got {:?}",
            hits.iter().map(|h| h.key.clone()).collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn mobile_build_recall_with_variations_short_circuits_to_recall() {
    let tmp = TempDir::new().expect("tempdir");
    let mem = mobile_build_memory(&tmp);

    mem.store(
        "law_dispose",
        "민법 제548조 계약 해제의 효과는 원상회복 의무를 포함한다.",
        MemoryCategory::Core,
        None,
    )
    .await
    .expect("seed");

    // Empty variations + no reranker attached → must route through the
    // cheaper recall() path without error.
    let empty: [String; 0] = [];
    let hits = mem
        .recall_with_variations("계약 해제 원상회복", &empty, 5, None)
        .await
        .expect("variations recall");
    assert!(
        hits.iter().any(|h| h.key == "law_dispose"),
        "mobile short-circuit must still return the seeded row"
    );
}

#[tokio::test]
async fn mobile_build_multi_variation_recall_fuses_via_rrf_without_reranker() {
    let tmp = TempDir::new().expect("tempdir");
    let mem = mobile_build_memory(&tmp);

    mem.store(
        "law_bond",
        "보증금 반환 청구 소송의 요건사실을 정리한다.",
        MemoryCategory::Core,
        None,
    )
    .await
    .expect("seed");

    // Agent-loop expanded variations. Without a reranker attached the
    // RRF path must still fuse hits across variations correctly.
    let variations = vec![
        "보증금 반환 소송".to_string(),
        "임차인 보증금 청구".to_string(),
    ];
    let hits = mem
        .recall_with_variations("보증금 반환", &variations, 5, None)
        .await
        .expect("multi-variation recall");
    assert!(
        hits.iter().any(|h| h.key == "law_bond"),
        "mobile multi-variation recall must return the seeded row via RRF without a reranker"
    );
}
