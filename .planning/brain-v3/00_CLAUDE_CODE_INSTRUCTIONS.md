# MoA v3.0 통합 변경 지시서 (Claude Code용)

> **작성일**: 2026-04-14
> **대상**: Claude Code (자동 구현 에이전트)
> **프로젝트 루트**: MoA 앱 (Rust + Tauri, SQLite 기반)
> **원칙**: 기존 MoA의 특허 구조(E2E 동기화, 에피소드↔온톨로지 이중 저장소)는 **절대 변경하지 않는다.** 본 지시서의 변경은 모두 **추가(additive)** 또는 **비파괴적 확장(non-breaking extension)** 이다.

---

## 0. 최우선 읽기 순서

본 지시서를 포함해 총 7개 파일이 같은 디렉토리에 있다. 반드시 아래 순서로 모두 읽고 시작한다.

1. `00_CLAUDE_CODE_INSTRUCTIONS.md` ← 본 파일 (최상위)
2. `01_ARCHITECTURE_PATCH.md` ← ARCHITECTURE.md에 반영할 변경분
3. `02_MASTER_PLAN.md` ← 제안 1~5 + 전화비서 v3 + 9카테고리 + 생성형 워크플로우 총정리
4. `03_migration_memory_timeline.sql` ← S2 SQLite 마이그레이션
5. `04_rrf_search.rs` ← S1 RRF Rust 스켈레톤
6. `05_workflow_engine.rs` ← 워크플로우 엔진 Rust 스켈레톤
7. `06_workflow_schema.json` ← YAML DSL의 JSON Schema

---

## 1. 이번 릴리즈(v3.0)의 전체 변경 범위 체크리스트

각 항목은 독립 PR로 분리 가능하며, 번호 순서대로 구현할 것.

### [A] 메모리/브레인 확장 (gbrain 장점 통합)

- [ ] **S1 · RRF 하이브리드 검색 도입** — `src/memory/hybrid.rs`(신규) + 기존 검색 함수 플래그 분기
  - 현재 `0.7*vec + 0.3*fts` 가중합을 유지하되, feature flag `search_mode = "rrf" | "weighted"` 로 A/B 전환
  - 기본값은 당분간 `"weighted"`, 벤치마크 통과 후 `"rrf"` 로 전환
  - 구현 스켈레톤: `04_rrf_search.rs` 참조

- [ ] **S2 · Timeline / Compiled Truth 이중 구조** — SQLite 스키마 확장 (ALTER TABLE + 신규 테이블)
  - 기존 `memories.content` 는 **삭제하지 않는다.** 하위호환을 위해 유지.
  - 신규 컬럼: `compiled_truth`, `truth_version`, `truth_updated_at`, `needs_recompile`
  - 신규 테이블: `memory_timeline` (append-only 증거), `phone_calls` (전화비서용)
  - 마이그레이션: `03_migration_memory_timeline.sql` 파일 그대로 실행
  - 델타 저널(`src/sync/`)에 새 테이블 기록 로직 추가 → 기존 E2E 동기화 파이프라인에 통합

- [ ] **S3 · 멀티 쿼리 확장 + 시맨틱 청킹** — `src/memory/query_expand.rs`(신규), `src/memory/chunk_semantic.rs`(신규)
  - 멀티 쿼리: Phase 1 전에 Haiku로 3~5개 변형 생성 → 병렬 검색 → RRF 융합
  - 시맨틱 청킹: 긴 문서(>2000자)에만 선택 적용. 짧은 대화 메모리는 기존 재귀 청킹 유지.

- [ ] **S4 · Dream Cycle (야간 consolidation)** — `src/memory/dream_cycle.rs`(신규 모듈)
  - 디바이스 idle 조건: (새벽 2~6시) AND (배터리>50% OR 충전 중) AND (네트워크 안정)
  - 리더 선출: 같은 사용자의 기기 중 `device_id` 최소값이 실행
  - 작업: (1) `needs_recompile=1` 메모리의 timeline→compiled_truth 재컴파일, (2) 온톨로지 엔티티 속성 강화, (3) 핫 캐시 재계산, (4) 중복 병합 제안 큐잉
  - 결과는 델타 저널에 기록 → 타 기기로 E2E 전파

### [B] 전화비서 카테고리 통합 (제안5 + 사용자 기존 기획안)

- [ ] 전화비서를 **9개 메인 카테고리 중 5번째**로 위치시킴 (아래 [C] 참조)
- [ ] gbrain 차용 기능 (전화 ↔ 브레인 양방향 연결) 4가지:
  - [ ] 수신 시 발신번호 → 온톨로지 Object 자동 매칭 → 관련 `compiled_truth` 로드
  - [ ] 로드된 진실을 시스템 프롬프트에 주입하여 Gemini Live 호출
  - [ ] 통화 종료 시 `memory_timeline` append + `phone_calls` row 생성 + 온톨로지 Action 생성
  - [ ] `needs_recompile=1` 세팅 → 다음 Dream Cycle에서 요약 재작성
- [ ] MoA 기존 특허 기능 그대로 유지: 위스퍼 디렉팅, 멀티스레드, 스마트 부재중, 실시간 피싱 탐지, SOS
- [ ] 상세: `02_MASTER_PLAN.md` Part B 참조

### [C] 9개 메인 카테고리 + 생성형 카테고리

- [ ] 메인 카테고리 **순서 및 이름 고정**:
  ```
  1. 일상업무   (daily)
  2. 쇼핑       (shopping)         ← 이번에 신규 추가
  3. 문서작업   (document)
  4. 코딩작업   (coding)
  5. 통역       (interpret)
  6. 전화비서   (phone)
  7. 이미지     (image)
  8. 음악       (music)
  9. 동영상     (video)
  ```
- [ ] 위 9개는 **Seed 카테고리**이며, 코드에 enum/const 로 하드코딩 (불변).
- [ ] 사용자가 UI의 `+` 버튼으로 **Custom 카테고리**를 추가할 수 있어야 한다 (DB에 저장, 순서 변경/삭제 가능).
  - 신규 테이블: `user_categories` (id, name, icon, parent_seed_id nullable, order_index, created_at)
  - `parent_seed_id` 가 있으면 해당 Seed 의 하위로 표시, 없으면 최상위
- [ ] 각 카테고리 내부에 **파생 워크플로우(Derived Workflows)** 가 0개 이상 존재
- [ ] 상세: `02_MASTER_PLAN.md` Part C 참조

### [D] 생성형 워크플로우 엔진 (사용자가 말로 만드는 루틴)

- [ ] 신규 테이블: `workflows`, `workflow_runs` (03_migration_memory_timeline.sql 참조)
- [ ] 신규 모듈: `src/workflow/` (엔진), `src/workflow/parser.rs` (YAML→IR), `src/workflow/exec.rs` (실행기)
- [ ] 스켈레톤: `05_workflow_engine.rs` 참조
- [ ] YAML 스키마: `06_workflow_schema.json` 로 validation
- [ ] 생성 경로 (Generative Path):
  - [ ] 음성 입력 → Intent Classifier (SLM) → "create_workflow" 의도 감지
  - [ ] Scaffolder (Claude Opus): 발화 + tool_registry + 유사 워크플로우 예시 → YAML 초안
  - [ ] Dry-run 검증 + 비용 추정
  - [ ] 사용자 확인 카드 + 음성 수정 수용
  - [ ] DB 저장 + 델타 저널 기록 → 타 기기 동기화
- [ ] 학습 루프: Dream Cycle 확장에서 `workflow_runs` 통계 분석 → 기본값 자동 제안

### [E] 변호사 프리셋 10종 (초기 탑재)

02_MASTER_PLAN.md Part E 의 10개 YAML 프리셋을 `resources/workflow_presets/` 에 그대로 저장하고, 첫 실행 시 `workflows` 테이블에 seed 로 import.

---

## 2. 절대 지켜야 할 제약 (Hard Constraints)

1. **DB는 SQLite 유지.** PGLite/Postgres 전환 금지. gbrain 설계를 참고하되 저장소는 MoA 기존 SQLite + sqlite-vec + FTS5 를 그대로 사용.
2. **기존 `memories.content` 컬럼 삭제 금지.** 모든 기존 쿼리 하위호환.
3. **특허 구조 불변**: 에피소드↔온톨로지 양방향 교차참조(Phase 2~4), 서버 비저장 E2E 동기화 구조는 그대로. 새 테이블도 델타 저널을 통해 동일한 동기화 경로를 타야 한다.
4. **운영자 API key 라우팅 플로우 불변**: ARCHITECTURE.md Section 1의 "★ MoA Core Workflow — Smart API Key Routing" 은 어떤 변경도 금지.
5. **LWW 대체 X, 보완 O**: 동기화 충돌 해결은 여전히 LWW. 다만 `memory_timeline` 은 append-only 이므로 충돌이 원천적으로 발생하지 않음. 이를 "LWW의 데이터 손실 완화" 로 문서화.
6. **gbrain 코드 직접 복사 금지.** 라이선스 확인 전에는 아이디어/패턴만 참조. 구현은 MoA 코드 스타일(Rust, tokio, sqlx/rusqlite)로 재작성.

---

## 3. 작업 순서 및 브랜치 전략

각 스프린트를 별도 브랜치로 진행. 모든 PR은 다음 검증 필수:
- `cargo test`
- `cargo clippy -- -D warnings`
- 기존 E2E 동기화 테스트 (`tests/sync_*.rs`) 통과
- Tauri 클라이언트 smoke test

| 순서 | 브랜치 | 범위 | 기간 |
|---|---|---|---|
| 1 | `feat/s1-rrf-search` | S1 | 1주 |
| 2 | `feat/s2-timeline-schema` | S2 (마이그레이션 + 동기화 통합) | 2주 |
| 3 | `feat/s3-query-expand-chunking` | S3 | 1주 |
| 4 | `feat/s4-dream-cycle` | S4 (로컬 단일 기기) | 2주 |
| 5 | `feat/phone-assistant-v3` | [B] 전화비서 브레인 통합 | 3주 |
| 6 | `feat/categories-9-seeds` | [C] 9 카테고리 + 사용자 `+` 추가 UI | 1주 |
| 7 | `feat/workflow-engine` | [D] 워크플로우 엔진 + YAML DSL | 2주 |
| 8 | `feat/workflow-generative-path` | [D] 음성→YAML Scaffolder | 2주 |
| 9 | `feat/lawyer-presets` | [E] 10종 프리셋 | 1주 |
| 10 | `feat/dream-cycle-workflow-learning` | S4 확장 (워크플로우 학습) | 2주 |

총 **17주 추정**. 단, S5(전화비서)와 S6(카테고리)/S7(워크플로우 엔진)은 의존성이 얕아 병렬화 가능.

---

## 4. 커밋 메시지 규칙

```
feat(memory): RRF hybrid search via reciprocal rank fusion (S1)

- Add src/memory/hybrid.rs implementing RRF with k=60
- Behind feature flag `memory.search_mode`; default=weighted
- Benchmark harness under benches/hybrid_search.rs (50 query set)

Refs: MoA v3.0 Plan §A / S1
```

- 제목: `<type>(<scope>): <summary> (<Stage-ID>)`
- 본문: 변경 요약 3줄 이내 + Refs

---

## 5. 변호사 프로덕션 앱 관점 고려사항

- 모든 `memory_timeline` 엔트리는 **법적 감사 로그** 로도 쓰일 수 있으므로 `source_ref` 필드를 절대 NULL 로 저장하지 말 것. call_uuid / message_id / file_hash 중 하나는 반드시 기록.
- PII(의뢰인 이름, 전화번호, 주민번호)는 **로컬에서만 평문**, 외부 전송 시(채널 릴레이, 웹채팅 포워드) 자동 마스킹 필터 적용.
- Custom 카테고리 이름도 PII 포함 가능 → 동기화 시에만 E2E 암호화 적용(기존 sync 경로 재활용 → 추가 작업 거의 없음).
- 워크플로우 감사: 모든 `workflow_runs` 는 입력/출력 SHA256 해시를 저장하여 사후 재현 가능해야 함.

---

## 6. 완료 기준 (Definition of Done)

한 스프린트가 완료되려면:
1. 신규 코드 단위 테스트 커버리지 ≥80%
2. ARCHITECTURE.md 해당 섹션 업데이트 (01_ARCHITECTURE_PATCH.md 의 해당 부분)
3. 사용자향 CHANGELOG 엔트리 작성
4. Tauri UI 변경 시 i18n(ko-KR, en-US) 동시 업데이트
5. 기존 테스트 100% 통과

---

## 7. 질문 시 확인 순서

Claude Code 가 구현 중 의사결정이 필요하면:
1. 먼저 `02_MASTER_PLAN.md` 의 해당 섹션 확인
2. 다음 본 파일 (00) 의 "절대 지켜야 할 제약" 확인
3. 그래도 모호하면 기존 MoA ARCHITECTURE.md 의 특허 청구항 확인 (Section 3)
4. 최종적으로 사용자(kjccjk77@gmail.com)에게 질의

---

**끝. 이제 `01_ARCHITECTURE_PATCH.md` 부터 순서대로 읽고 작업 시작.**
