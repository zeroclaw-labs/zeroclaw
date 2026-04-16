# Next-session handoff prompt

Paste the block below into the next Claude session to resume work.

---

```
이번 세션은 이전 세션의 MoA Vault/Memory 아키텍처 하드닝 작업을 이어서
진행합니다.

## 이전 세션에서 완료된 것 (feat/document-pipeline-overhaul 브랜치, GitHub 푸시 완료)

최근 commit: ec0d8968 docs(arch): §6E-8 session summary
이전 세션 시작점: 137c846a

18개 원자 커밋으로 §6E-7 로드맵 PR #1/#4/#5/#6/#7/#8/#9 전부 구현 완료 +
대부분의 "잔여" 항목까지 처리. 자세한 내용은 `docs/ARCHITECTURE.md §6E-8`
Session Summary 섹션 참조.

현재 테스트: vault 126 + memory 396 + sync 68 + phone 20 + ontology 27
+ billing 74 = **711 pass / 0 fail** (이전 세션 518 → +193).

## 이번 세션에서 해야 할 "후속" 작업

`docs/ARCHITECTURE.md §6E-8`에 ⏳ 후속 마커가 붙은 다음 항목들을 구현:

### 우선순위 1 — 즉시 가치가 있고 환경 구축이 간단한 작업

1. **PR #4 Reranker on/off 정확도 비교**
   - 절차: `cargo run --bin moa_eval -- --set law --emit-retrieval
     /tmp/r_off.jsonl --top-k 5` 실행 (reranker 없음).
   - `cargo build --features embedding-local` 후 모델 다운로드
     (fastembed 5.8 + BGE-reranker-v2-m3, ~560MB).
   - SqliteMemory에 reranker 주입할 수 있는 eval harness 변형 추가
     (지금은 주입 경로가 없음 — `src/bin/moa_eval.rs`에 `--enable-rerank`
     플래그 추가 필요, `SqliteMemory::set_reranker(Arc::new(
     create_reranker("bge-reranker-v2-m3")))` 호출).
   - `_on.jsonl` vs `_off.jsonl`의 recall@5 / precision@5 / MRR 비교.
   - 수락 기준: law 도메인에서 ≥5pt MRR 개선 (로드맵 스펙).

2. **PR #9 100 객체 + 200 링크 <1s 벤치**
   - `benches/community_detection.rs` 신설 (criterion 사용,
     `benches/agent_benchmarks.rs`가 이미 criterion 씀).
   - 랜덤 연결된 100-node / 200-edge 그래프 → `detect_communities`
     호출 → p99 <1s 확인.
   - 현재 알고리즘은 LPA (O(V+E)·iter), 100-node에선 sub-ms 예상.

3. **PR #1 CPU 32배치 <2s 벤치**
   - `benches/` 아래 feature-gated bench 추가
     (`#[cfg(feature = "embedding-local")]`).
   - 32개 한국어 문장 배치 → `embedder.embed(&texts)` 지연 측정.
   - 모델이 로컬 캐시되어야 함 (`~/.moa/embedding-models/bge-m3/`).

### 우선순위 2 — 프로토콜 / 스키마 변경 (careful)

4. **PR #7 sync protocol version bump** (HLC를 primary 정렬 키로 전환)
   - `src/sync/protocol.rs`에서 SyncPayload/VersionVector 스키마 확인.
   - `updated_at` 문자열 비교 대신 `updated_at_hlc` HLC 비교로 정렬 로직
     교체. Hlc::parse + PartialOrd 활용.
   - sync protocol 버전 상수 bump, 구버전 피어 fallback 경로
     (old 버전이면 기존 `updated_at` 정렬 유지).
   - 테스트: 서로 다른 두 노드에서 5분 시계 드리프트 상황 시뮬레이션,
     HLC 정렬로 결과가 일관됨을 확인.

5. **PR #9 Leiden 알고리즘 교체** (optional — LPA가 충분하면 skip)
   - `src/ontology/community.rs::detect_communities` 내부만 교체하면
     됨 (trait/시그니처 유지). `leiden-rs` 또는 `petgraph` 기반 구현.
   - 1000 object 그래프에서 LPA 대비 modularity 점수 비교.
   - 개선이 미미하면 PR 보류.

### 우선순위 3 — 프론트엔드 / UI 작업 (Tauri)

6. **PR #1 Tauri 모델 다운로드 UI**
   - `clients/` 아래 Tauri 앱이 있으면 거기서, 없으면 web/에서.
   - `LocalFastembedProvider::try_new`의 다운로드 이벤트를 Tauri Event
     로 프론트엔드에 전달 (fastembed 5.8의 download progress API 활용).
   - 1.1GB 다운로드 중 진행률 바 + ETA 표시.

7. **PR #6 아카이브 UI**
   - `memories WHERE archived = 1` 리스트 뷰 + 행별 "복구" 버튼.
   - 복구 = `UPDATE memories SET archived = 0 WHERE id = ?`.
   - `consolidated_memories` 조인해서 "이 아카이브는 community X로
     합쳐짐" 표시.

### 우선순위 4 — 데이터 큐레이션 (사용자 입력 필요)

8. **PR #8 코퍼스 확장** — 현재 110 → 스펙 목표 180 (ko 100 / en 50 / law 30)
   - `tests/evals/{corpus,golden_ko,golden_en}.jsonl`에 50 ko + 20 en 추가.
   - 사용자의 실제 사용 패턴 기반 쿼리가 이상적 (legal 도메인 특화).
   - law는 이미 30개 달성했으니 목표 완수.

## 작업 원칙 (이전 세션에서 확립, 동일하게 유지)

- 각 작업은 독립 원자 커밋, Conventional Commits 형식
- `Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>` trailer
- 모든 코드는 테스트와 함께 land, 회귀 0 유지 (`cargo test --lib`
  vault + memory + sync + phone + ontology + billing 순차 실행)
- 기존 특허 구조(Patent 1/2/3/4) 훼손 금지
- `docs/ARCHITECTURE.md §6E-8` 테이블에서 완료 항목을 ⏳ → ✅로 승격

## 시작 전 검증

1. `git log --oneline -5` — 최상단이 `ec0d8968 docs(arch): §6E-8…`
2. `git status` — clean (no uncommitted changes)
3. `cargo test --lib memory:: 2>&1 | tail -3` — 396 pass / 0 fail
4. `docs/ARCHITECTURE.md §6E-8` Session Summary 섹션을 먼저 읽고 전체
   맥락 숙지

그 다음 우선순위 1의 작업부터 시작. 각 작업 완료 시 commit → push →
§6E-8 테이블 업데이트.

## 참고 파일 포인터

- 전체 로드맵: `docs/ARCHITECTURE.md §6E-7`
- 이번 세션 요약: `docs/ARCHITECTURE.md §6E-8`
- 보안 문서: `docs/security/embedding-privacy.md` (vec2text + SQLCipher)
- 평가 하네스: `tests/evals/README.md`
- 핵심 테스트 사이트:
  - `src/memory/sqlite.rs::tests::read_pool_*` (r2d2)
  - `src/memory/sqlite.rs::tests::store_stamps_monotonic_hlc_*` (HLC)
  - `src/memory/embedding/local_fastembed.rs::tests::embed_is_deterministic*` (feature-gated)
  - `src/memory/consolidate.rs::tests::*` (단일-링크 클러스터링)
  - `src/ontology/community.rs::tests::*` (LPA)
  - `src/vault/scheduler.rs::tests::community_summariser_*` (LLM 연동)
```

---

**사용법**: 다음 세션을 열면 위 블록을 첫 메시지로 붙여넣고 시작하면 됨.
이 파일은 `.planning/` 아래에 있어 repo에 커밋되어도 배포 artifact에는
포함되지 않음.
