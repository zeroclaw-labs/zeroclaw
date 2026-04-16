# ARCHITECTURE.md 변경 패치 (v2 → v3.0)

> Claude Code 에게: 이 파일은 **ARCHITECTURE.md 에 직접 반영해야 할 변경분**이다. 아래 각 섹션의 지시에 따라 원본 파일을 수정하라. 원본이 너무 커서 전체 재작성은 위험하므로 **섹션 단위 ALTER / INSERT / APPEND** 방식으로 처리한다.

---

## Patch 0. 문서 헤더 업데이트

원본 상단의 Date/Status 교체:

```diff
- > **Date**: 2026-03-01
- > **Status**: Living document — updated with each major feature milestone
+ > **Date**: 2026-04-14
+ > **Version**: 3.0 (gbrain integration + 9 categories + generative workflows)
+ > **Status**: Living document — updated with each major feature milestone
```

---

## Patch 1. Section 1 (Product Vision) 말미에 추가

"### Core Thesis" 블록 뒤, "---" 앞에 다음을 APPEND:

```markdown
### Task Categories (v3.0)

MoA 의 모든 사용자 요청은 9개의 **Seed 카테고리** 중 하나 이상에 속한다:

| # | 카테고리 | 영문 키 | 대표 도구 |
|---|---------|--------|-----------|
| 1 | 일상업무 | `daily` | calendar, memo, search, web |
| 2 | 쇼핑 | `shopping` | web, browser, price-compare, receipt |
| 3 | 문서작업 | `document` | docx, pdf, xlsx, pptx |
| 4 | 코딩작업 | `coding` | shell, editor, git, linter |
| 5 | 통역 | `interpret` | voice_interpret (Gemini Live) |
| 6 | 전화비서 | `phone` | phone_router, stt, whisper_direct |
| 7 | 이미지 | `image` | imagegen, image_edit |
| 8 | 음악 | `music` | music_gen, music_edit |
| 9 | 동영상 | `video` | video_gen, video_edit |

**Seed 카테고리는 코드에 하드코딩**(`src/categories/seed.rs`)되며 삭제/이름변경 불가.
사용자는 UI의 `+` 버튼으로 **Custom 카테고리**를 생성할 수 있다 (`user_categories` 테이블).

각 카테고리는 다시 여러 **Derived Workflows**(파생 워크플로우)로 세분화되며,
사용자가 **음성으로 요청하여 AI가 자동 생성** 할 수 있다 (Section 6 참조).
```

---

## Patch 2. Section 3 (Patent) 뒤에 신규 Section 4 삽입

기존 "### Patent 2: Bidirectional Cross-Referenced Dual-Store AI Memory System" 블록 이후, 다음 주요 섹션이 나오기 직전에 APPEND:

```markdown
---

## 3b. Brain Layer v3.0 (gbrain 장점 통합)

> **개요**: 기존 에피소드↔온톨로지 이중 저장소 위에,
> gbrain 에서 검증된 "Compiled Truth + Timeline" 패턴과 RRF 검색, Dream Cycle 을
> **추가 레이어**로 통합한다. 기존 특허 구조는 그대로 보존되며, 본 레이어는
> 기존 `memories` 테이블의 **비파괴적 확장**이다.

### 3b-1. Compiled Truth + Timeline 이중 구조

```
┌─────────────────────────────────────────────────────────────┐
│  memories 테이블 (기존)                                      │
│   ├─ content: 원본 콘텐츠 (하위호환 유지)                     │
│   ├─ embedding: 벡터                                         │
│   └─ [신규] compiled_truth: "현재까지의 최선의 요약" (LLM)    │
│                                                             │
│  memory_timeline 테이블 (신규, append-only)                  │
│   ├─ memory_id → memories.id                                │
│   ├─ event_type: 'call'/'chat'/'doc'/'manual'/'workflow'    │
│   ├─ content: 원본 증거 (절대 수정 X)                         │
│   └─ source_ref: call_uuid / message_id / file_hash          │
└─────────────────────────────────────────────────────────────┘
```

- `compiled_truth` 는 **요약**, `timeline` 은 **출처**. LLM 답변 시 요약을
  주고 출처(memory_timeline.id)를 각주로 인용 → **할루시네이션 방지 + 법적 감사 가능**.
- `timeline` 은 append-only 이므로 동기화 시 LWW 충돌이 발생하지 않아,
  기존에 지적된 "LWW 데이터 손실" 리스크를 구조적으로 완화한다.

### 3b-2. RRF 하이브리드 검색

기존 `0.7 * vec + 0.3 * fts` 가중합을 **Reciprocal Rank Fusion** 으로 교체:

```
rrf_score(doc) = Σ 1 / (k + rank_i)    (k = 60, i ∈ {vec, fts, ...queries})
```

점수 스케일 차이에 영향받지 않으므로 BM25/cosine 혼합에서 공정함.
Phase 1 검색에만 적용 (Phase 2 온톨로지 전문검색은 그대로).

### 3b-3. Multi-Query Expansion

Phase 1 진입 전 Haiku 가 원 질의를 3~5개로 변형:
- 법률 도메인 예: "이혼 소송" → {"이혼", "협의이혼", "재산분할", "위자료"}
각각 병렬 검색 후 RRF 로 융합.

### 3b-4. Semantic Chunking (선택 적용)

긴 문서(>2000자)만 Savitzky-Golay 필터 기반 주제 경계 감지로 청킹.
짧은 대화 메모리는 기존 재귀 청킹 유지.

### 3b-5. Dream Cycle (야간 Consolidation)

디바이스 idle 시 로컬에서 자동 실행되는 학습/정리 루프.

```
조건: (02:00 ≤ 현재시각 ≤ 06:00) AND (battery ≥ 50% OR charging) AND online
리더 선출: 같은 사용자의 device_id 최솟값 1대만 실행
작업:
  1. needs_recompile = 1 인 memory 의 timeline → compiled_truth 재컴파일
  2. 온톨로지 엔티티 속성 강화 (recall_count 기반)
  3. 핫 캐시 재계산 (실제 최근 호출 패턴)
  4. 중복 병합 제안 큐잉 (유사도 > 0.95)
  5. workflow_runs 통계 분석 → 기본값 자동 제안
결과: 델타 저널 기록 → 타 기기 E2E 전파
```

"Hot cache 5분 TTL 갱신 지연" 이슈의 근본 해결책.

### 3b-6. 전화비서 ↔ 브레인 양방향 연결

전화 수신/종료 시 자동 수행:
- 수신: 발신번호 → 온톨로지 Object 매칭 → 해당 memories 의 compiled_truth → 시스템 프롬프트 주입
- 종료: STT 전사본 → memory_timeline append, 온톨로지 Action 생성, needs_recompile=1

자세한 흐름은 Section 5 (전화비서) 참조.
```

---

## Patch 3. 신규 Section 5 — 전화비서 카테고리 (v3.0)

위 Patch 2 뒤에 APPEND:

```markdown
---

## 5. 전화비서 카테고리 아키텍처

### 5-1. 기능 매트릭스 (MoA 기존 + gbrain 차용)

| # | 기능 | 출처 | 구현 위치 |
|---|------|------|-----------|
| 1 | 위스퍼 디렉팅 | MoA 특허 | `src/phone/whisper_direct.rs` |
| 2 | 멀티스레드 동시 수신 | MoA 특허 | `src/phone/multi_thread.rs` |
| 3 | 스마트 부재중 응대 | MoA | `src/phone/missed_call.rs` |
| 4 | 통화 녹음 + STT + 요약 + GPS | MoA | `src/phone/transcribe.rs` |
| 5 | 자연어 검색 "지난주 김철수…" | MoA + **RRF** | `src/memory/hybrid.rs` 재활용 |
| 6 | 캘린더 충돌 감지/자동 등록 | MoA | 기존 calendar 도구 |
| 7 | 실시간 피싱/스팸 탐지 | MoA | `src/phone/phishing_detect.rs` |
| 8 | SOS 자동 감지 + AI 대리 신고 | MoA | `src/phone/sos.rs` |
| 9 | 24h 포그라운드 상시 실행 | MoA | 플랫폼별 native (Android: ForegroundService) |
| 10 | **발신자 → 온톨로지 매칭** | gbrain 차용 | `src/phone/caller_match.rs` |
| 11 | **compiled_truth 시스템 프롬프트 주입** | gbrain 차용 | `src/phone/context_inject.rs` |
| 12 | **통화 종료 → timeline/Action 생성** | gbrain 차용 | `src/phone/post_call.rs` |
| 13 | **Dream Cycle 재컴파일** | gbrain 차용 | `src/memory/dream_cycle.rs` |

### 5-2. 통화 흐름

```
┌─── 수신 ───────────────────────────────────┐
│ 1. 발신번호 추출                            │
│ 2. caller_match → ontology_objects 조회    │
│ 3. 매칭 O → 관련 memories.compiled_truth   │
│         → 시스템 프롬프트에 주입             │
│    매칭 X → 익명 프로토콜                    │
│ 4. 병렬 스레드 기동                         │
│    ├ Gemini Live 메인 대화                 │
│    ├ 위스퍼 리스너 (저속 STT)                │
│    ├ 피싱 탐지기 (온디바이스 SLM)            │
│    └ SOS 키워드 감지기                      │
│ 5. 다국어 자동 감지 + 원어민 교정 발화        │
└────────────────────────────────────────────┘
               │
               ▼
┌─── 종료 ───────────────────────────────────┐
│ 6. phone_calls row 생성 (통화 메타)         │
│ 7. memory_timeline append (전사본 + GPS)    │
│ 8. 일정/약속 감지 → 캘린더 자동 등록          │
│ 9. 온톨로지 Action 생성 ("전화상담")         │
│ 10. memories.needs_recompile = 1            │
│ 11. 델타 저널 기록 → 타 기기 동기화           │
└────────────────────────────────────────────┘
               │
               ▼
┌─── Dream Cycle (당일 새벽) ─────────────────┐
│ 12. timeline → compiled_truth 재작성        │
│ 13. 발신자 Object 속성 강화                 │
└────────────────────────────────────────────┘
```

### 5-3. 주요 테이블

`phone_calls`, `memory_timeline` — SQLite 스키마는
`migrations/2026_04_v3_timeline.sql` 참조.
```

---

## Patch 4. 신규 Section 6 — 9개 카테고리 + 생성형 워크플로우

Patch 3 뒤에 APPEND:

```markdown
---

## 6. 카테고리 체계 + 생성형 워크플로우

### 6-1. Seed 카테고리 (9개, 하드코딩, 불변)

```rust
// src/categories/seed.rs
pub const SEED_CATEGORIES: &[(&str, &str, &str)] = &[
    ("daily",      "일상업무", "🏠"),
    ("shopping",   "쇼핑",     "🛒"),
    ("document",   "문서작업", "📝"),
    ("coding",     "코딩작업", "💻"),
    ("interpret",  "통역",     "🎙️"),
    ("phone",      "전화비서", "☎️"),
    ("image",      "이미지",   "🎨"),
    ("music",      "음악",     "🎵"),
    ("video",      "동영상",   "🎬"),
];
```

### 6-2. Custom 카테고리 (사용자 `+` 추가)

```sql
CREATE TABLE user_categories (
  id INTEGER PRIMARY KEY,
  uuid TEXT UNIQUE NOT NULL,
  name TEXT NOT NULL,
  icon TEXT,
  parent_seed_key TEXT,    -- 'document' 등; NULL 이면 최상위
  order_index INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
```

- UI: 카테고리 그리드 말미에 `+` 버튼 → 음성 또는 텍스트로 이름/아이콘/상위 Seed 지정
- 순서 변경: 드래그 or 음성 ("쇼핑을 맨 앞으로")
- 삭제: 내부 워크플로우가 있으면 이동 여부 확인

### 6-3. Workflow (파생 워크플로우)

각 카테고리에 0개 이상의 워크플로우가 속함. YAML DSL 로 선언적 기술.

```sql
CREATE TABLE workflows (
  id INTEGER PRIMARY KEY,
  uuid TEXT UNIQUE NOT NULL,
  parent_category TEXT NOT NULL,   -- seed key 또는 user_category uuid
  name TEXT NOT NULL,
  description TEXT,
  icon TEXT,
  spec_yaml TEXT NOT NULL,
  trigger_type TEXT,               -- 'voice'/'schedule'/'event'/'manual'
  trigger_config_json TEXT,
  version INTEGER DEFAULT 1,
  parent_workflow_id INTEGER,
  created_by TEXT,                 -- 'user'/'ai_generated'/'preset'
  usage_count INTEGER DEFAULT 0,
  last_used_at INTEGER,
  is_pinned INTEGER DEFAULT 0,
  created_at INTEGER,
  updated_at INTEGER
);

CREATE TABLE workflow_runs (
  id INTEGER PRIMARY KEY,
  workflow_id INTEGER NOT NULL,
  started_at INTEGER,
  ended_at INTEGER,
  status TEXT,
  input_json TEXT,
  input_sha256 TEXT,               -- 법적 감사용
  output_ref TEXT,
  output_sha256 TEXT,
  feedback_rating INTEGER,
  cost_tokens_in INTEGER,
  cost_tokens_out INTEGER
);
```

### 6-4. 생성형 워크플로우 (Voice → YAML)

```
사용자 음성: "의뢰인 전화 끝나면 매번 상담일지 자동으로 써줘"
    ▼
① Intent Classifier (로컬 SLM) → action=create_workflow
② Scaffolder (Claude Opus) → YAML 초안 생성
    입력: 발화 + tool_registry + 유사 워크플로우 샘플 3개 + 온톨로지 관련 Object
③ Dry-run 검증 (schema + mock 실행 + 비용 추정)
④ 사용자 확인 카드 + 음성 수정 수용 (YAML patch)
⑤ workflows 저장 + FTS 인덱싱 + 델타 저널 기록 → 타 기기 동기화
⑥ Dream Cycle 에서 workflow_runs 통계 분석 → 기본값/실패원인 자동 개선 제안
```

### 6-5. 안전장치

- 권한 화이트리스트: 워크플로우 실행 도구는 카테고리별 승인 범위 내로 제한
- 비용 상한: `max_tokens_per_run`, `max_llm_calls_per_run` 필수
- PII 마스킹: 외부 전송 전 자동 필터
- 버전 관리: 수정 시 `parent_workflow_id` 로 이전 버전 보존 (rollback 가능)
- 감사 로그: `workflow_runs.input_sha256`/`output_sha256` 필수 기록
```

---

## Patch 5. Section "발견된 개선 포인트" 업데이트

원본의 개선 포인트 표에서 LWW 행과 핫캐시 5분 행을 **Resolved in v3.0** 처리:

```diff
- │ 낮 │ 동기화 충돌 해결이 LWW 단일 전략 │ 동시 편집 시 데이터 손실 가능…
+ │ ✅ Resolved v3.0 │ memory_timeline append-only 도입으로 충돌 원천 제거 │ 기존 memories.content 는 LWW 유지 (하위호환). 신규 증거는 timeline 에만 쌓이므로 손실 없음.

- │ 낮 │ 핫 캐시 갱신 주기 5분 │ 빈번한 프로필 변경 시 지연…
+ │ ✅ Resolved v3.0 │ Dream Cycle 이벤트 기반 갱신 + idle 재계산 │ 5분 TTL 유지하되 중요 변경은 즉시 invalidate.
```

---

## Patch 6. 목차 추가

원본 최상단 목차(있다면)에 다음 항목 삽입:

```markdown
- 3b. Brain Layer v3.0 (gbrain integration)
- 5. 전화비서 카테고리 아키텍처
- 6. 카테고리 체계 + 생성형 워크플로우
```

---

## 적용 검증

패치 적용 후:
- [ ] `grep -c "v3.0" ARCHITECTURE.md` ≥ 8
- [ ] Section 3b, 5, 6 가 모두 존재
- [ ] 기존 Section 1~3 의 특허 청구항 문구 **변경 없음** 확인
- [ ] 기존 API key 라우팅 흐름도 **변경 없음** 확인

**끝.**
