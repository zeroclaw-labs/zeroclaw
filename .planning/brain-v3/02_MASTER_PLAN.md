# MoA v3.0 Master Integration Plan

> **목적**: 제안 1~5 (gbrain 통합) + 전화비서 v3 + 9개 카테고리 + 생성형 워크플로우를 단일 문서로 총정리.
> **대상**: 제품/설계 리뷰 + Claude Code 구현 가이드

---

## Part A. 제안 1~4: Brain Layer 확장

### A-1. 제안 1 — Compiled Truth + Timeline 이중 구조

### 동기

- gbrain 의 핵심 패턴. 한 주제(page)를 "현재 최선의 요약(compiled_truth)" + "append-only 증거(timeline)" 로 분리.
- 변호사 업무에 직결: "의뢰인 A 현황"을 요약 + 원본 상담/통화 기록 인용으로 답변 → 할루시네이션 방지 + 법적 감사 가능.

### MoA 통합 방식

- 기존 `memories` 테이블에 **컬럼 추가** (삭제 없음)
  - `compiled_truth TEXT NULL`
  - `truth_version INTEGER DEFAULT 0`
  - `truth_updated_at INTEGER NULL`
  - `needs_recompile INTEGER DEFAULT 0`
- 신규 테이블 `memory_timeline` (append-only)
- 기존 `content` 필드는 하위호환 유지.
- 온톨로지(`ontology_objects/links/actions`)는 그대로 유지 → **양방향 교차참조 특허 불변**.

### 동기화 통합

- `memory_timeline` 을 델타 저널(`src/sync/journal.rs`)의 대상 테이블에 추가.
- append-only 특성상 LWW 충돌 없음 → 기존에 지적된 "LWW 데이터 손실" 리스크 자연 완화.

**마이그레이션**: `03_migration_memory_timeline.sql` 참조.

---

### A-2. 제안 2 — RRF (Reciprocal Rank Fusion) 하이브리드 검색

**현재**: `score = 0.7 * vector_score + 0.3 * keyword_score`
**문제**: BM25 점수와 cosine 유사도 스케일이 달라 한쪽 편향.

### RRF 공식

```
rrf(doc) = Σ_{i ∈ rankers}  1 / (k + rank_i(doc))    (k = 60 표준)
```

### 도입 전략 — Feature Flag A/B

- 신규 config: `memory.search_mode ∈ {"weighted", "rrf"}`, 기본값 `"weighted"`
- 플래그 `"rrf"` 로 전환 시 RRF 경로 사용
- 벤치마크 harness 통과 (변호사 질의 50개, recall@10 향상 ≥ 5%) 시 기본값 전환

**구현 위치**: `src/memory/hybrid.rs` (신규 — `04_rrf_search.rs` 스켈레톤)

---

### A-3. 제안 3 — Multi-Query Expansion + Semantic Chunking

### Multi-Query Expansion

- Phase 1 검색 진입 전 Claude Haiku 호출 → 질의를 3~5개 변형으로 확장
- 예: "이혼 소송" → {"이혼 절차", "협의이혼", "재산분할 기준", "위자료 판례"}
- 각 변형을 병렬 검색, 결과를 **RRF 로 최종 융합**
- 캐시: 최근 24h 동일 질의는 재사용

### Semantic Chunking (선택 적용)

- `>2000자` 문서에만 적용
- Savitzky-Golay 필터로 임베딩 거리 변화를 smoothing → 주제 경계 감지
- 짧은 대화/메모리는 기존 재귀 청킹 유지 (속도 우선)

### 구현 위치

- `src/memory/query_expand.rs` (신규)
- `src/memory/chunk_semantic.rs` (신규)

---

### A-4. 제안 4 — Dream Cycle (야간 자동 학습)

**트리거 조건** (AND)
- 로컬 시각 02:00 ~ 06:00
- 배터리 ≥ 50% 또는 충전 중
- 네트워크 안정(최근 5분 rtt < 500ms)

**리더 선출**: 동일 사용자의 모든 디바이스 중 `device_id` 최솟값 1대만 실행. 델타 저널로 결과 전파.

### 작업 목록

1. `needs_recompile = 1` 인 메모리의 `memory_timeline` → `compiled_truth` 재작성
2. 온톨로지 `Object` 속성 강화 (`recall_count` 상위 N개 LLM 재정리)
3. 핫 캐시 재계산 (최근 실제 호출 패턴 기반) — 기존 5분 TTL 이슈 해결
4. 중복 병합 제안 큐잉 (유사도 ≥ 0.95 메모리 쌍)
5. **Workflow 학습 루프** (Part D 참조):
   - `workflow_runs` 실패율 > 20% 워크플로우에 대해 원인 분석 + 수정안 제시
   - 반복 파라미터 조정 패턴 감지 → 기본값 자동 업데이트 제안
   - 유사 워크플로우 3개 이상 → 상위 abstraction 제안

**구현 위치**: `src/memory/dream_cycle.rs` (신규 모듈)

---

## Part B. 전화비서 카테고리 통합 기획 (제안 5 통합)

### B-1. 통합 전 비교

| 축 | MoA 기존 기획안 | gbrain (Twilio) | 통합 방향 |
|----|----------------|-----------------|----------|
| 통화 중 개입 | 위스퍼 디렉팅, 멀티스레드, SOS, 피싱 탐지 | 없음 | **MoA 유지** |
| 음성 스택 | Gemini Live (통역 모듈과 공유) | OpenAI Realtime | **Gemini Live 재활용** |
| 발신자 식별 | 기본 연락처 | 브레인 페이지 매칭 | **gbrain 방식 채용** (온톨로지 Object 매칭) |
| 컨텍스트 주입 | 제한적 | compiled_truth 주입 | **gbrain 방식 채용** |
| 통화 후 기록 | 녹음 + STT + 요약 | 브레인 페이지 자동 생성 | **융합**: MoA 녹음 + gbrain timeline/Action |
| 재학습 | 없음 | Dream Cycle | **Dream Cycle 채용** |
| 채널 위치 | 독립 앱 기획 | 별도 서비스 | **MoA 9 카테고리 중 5번째로 편입** |
| 해외 번호 지원 | 없음 | Twilio | **선택**: Twilio 채널 추가 (국내는 OS native) |

### B-2. 중복 제거 결과 — 최종 기능 목록

### 유지 (MoA 고유 특허 기능)

1. 위스퍼 디렉팅 (통화 중 작은 소리 지시 → 유창한 원어민 발화)
2. 멀티스레드 동시 수신 (스팸 자동종료 / VIP 응대 / 토스트 알림)
3. 스마트 부재중 응대
4. 통화 녹음 + STT + 요약 + GPS
5. 캘린더 충돌 감지 + 자동 등록
6. 실시간 피싱/스팸 탐지 + 커뮤니티 공유
7. SOS 자동 감지 + AI 대리 신고 + 행동 지침 안내
8. 24h 포그라운드 상시 실행 + 배터리 최적화 예외
9. 하이브리드 모드 (클라우드/온디바이스 자동 전환)

### 신규 (gbrain 차용)

10. 발신번호 → 온톨로지 Object 자동 매칭
11. 매칭된 Object 의 `compiled_truth` → 시스템 프롬프트 주입
12. 통화 종료 → `memory_timeline` append + `phone_calls` 메타 + 온톨로지 `Action` 생성
13. Dream Cycle 에서 자동 요약 재작성

### 강화 (기존 기능의 Brain Layer 활용)

14. 자연어 검색 "지난주 김철수 통화 찾아줘" → RRF 검색 + timeline 출처 인용
15. "신규 의뢰인 접수 대본" 같은 **전화비서 워크플로우** 자동 실행 (Part D)

### B-3. 통화 흐름 상세

```
[수신]
├─ 번호 정규화 (E.164)
├─ caller_match.rs:
│   ontology_objects WHERE attributes @> {phone_numbers: [...]}
│   → 매칭 없음 → "익명 발신자" 프로토콜 (신규 접수 대본 트리거)
│   → 매칭 있음 → object_id 추출
├─ context_inject.rs:
│   SELECT compiled_truth, timeline 최근 5건
│   FROM memories JOIN memory_timeline
│   WHERE linked_object_id = :object_id
│   → 요약 2000 토큰 이내로 system prompt 구성
├─ 병렬 스레드 기동:
│   ├─ Gemini Live 대화 (메인)
│   ├─ whisper_direct.rs (저속 STT + LLM 교정)
│   ├─ phishing_detect.rs (온디바이스 SLM, 슬라이딩 윈도우)
│   └─ sos.rs (키워드 매칭 + 급박성 분류)

[종료]
├─ phone_calls row 생성 (call_uuid, caller_object_id, gps, started/ended, risk_level)
├─ memory_timeline append:
│   event_type='call', source_ref=call_uuid,
│   content=전사본, metadata={duration, gps, 발신번호}
├─ post_call.rs:
│   ├─ 일정 감지 → calendar 도구로 자동 등록
│   ├─ ontology_actions INSERT (type='phone_call', target=object_id)
│   └─ memories.needs_recompile = 1
└─ 델타 저널 기록 → 타 기기 E2E 전파

[Dream Cycle (새벽)]
├─ needs_recompile=1 메모리 순회
├─ LLM 호출: compiled_truth 재작성 (기존 + 신규 timeline 요약)
├─ ontology_objects 속성 강화 (예: "주요 안건: 이혼소송 진행중")
└─ truth_version++, needs_recompile=0
```

### B-4. 신규/확장 모듈 트리

```
src/phone/
├── mod.rs
├── whisper_direct.rs        (기존 기획 구현)
├── multi_thread.rs          (기존)
├── missed_call.rs           (기존)
├── transcribe.rs            (기존 STT)
├── phishing_detect.rs       (기존)
├── sos.rs                   (기존)
├── caller_match.rs          (신규 gbrain 차용)
├── context_inject.rs        (신규 gbrain 차용)
├── post_call.rs             (신규 gbrain 차용)
└── channels/
    ├── android_native.rs    (ForegroundService 연동)
    ├── ios_native.rs        (CallKit 연동)
    └── twilio.rs            (선택, 해외번호)
```

---

## Part C. 9개 메인 카테고리 + 사용자 Custom 카테고리

### C-1. 순서 및 키 (하드코딩, 불변)

```
1. 일상업무   daily       🏠
2. 쇼핑      shopping     🛒    ← v3.0 신규
3. 문서작업   document    📝
4. 코딩작업   coding      💻
5. 통역      interpret    🎙️
6. 전화비서   phone       ☎️
7. 이미지    image        🎨
8. 음악      music        🎵
9. 동영상    video        🎬
```

**구현 위치**: `src/categories/seed.rs` (const 배열). enum 으로도 생성:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SeedCategory {
    Daily, Shopping, Document, Coding,
    Interpret, Phone, Image, Music, Video,
}
```

### C-2. 카테고리별 대표 도구 및 기본 워크플로우

| 카테고리 | 기본 도구 | 예시 프리셋 워크플로우 |
|---------|----------|---------------------|
| 일상업무 | calendar, memo, search, web | 기일 브리핑, 주간 진행 리포트 |
| 쇼핑 | web, browser, price-compare, receipt OCR | 최저가 검색 + 쿠폰 자동 적용, 영수증 스캔 가계부 |
| 문서작업 | docx, pdf, xlsx, pptx | 상담일지, 소장 초안, 계약서 리뷰 |
| 코딩작업 | shell, git, linter, editor | 코드 리뷰, 테스트 자동 생성 |
| 통역 | voice_interpret | 실시간 회의 통역, 영상 자막 |
| 전화비서 | phone_* | 신규 의뢰인 접수, VIP 응대 |
| 이미지 | imagegen, image_edit | 증거 사진 주석, 포스터 |
| 음악 | music_gen, music_edit | BGM 생성, 녹음 정리 |
| 동영상 | video_gen, video_edit | 영상 요약, 자막 합성 |

### C-3. Custom 카테고리

### UI 동작

- 카테고리 그리드 우하단에 `＋` 버튼
- 터치 → 모달: (이름, 아이콘 선택, 상위 Seed 선택 optional)
- 음성: "일상업무 아래에 '건강관리' 카테고리 추가" → 즉시 생성

**DB**: `user_categories` 테이블 (01_ARCHITECTURE_PATCH.md Patch 4 참조)

**동기화**: 델타 저널에 포함 → 모든 기기에 E2E 암호화 후 전파

### 제약

- Seed 키와 이름 충돌 금지 (validation)
- 삭제 시 하위 워크플로우 있으면 "이동할 카테고리" 선택 강제
- 최대 100개 (스팸 방지)

---

## Part D. 생성형 워크플로우 엔진

### D-1. 데이터 모델

`workflows`, `workflow_runs` 테이블 → `03_migration_memory_timeline.sql` 참조.

### D-2. YAML DSL 규격

- JSON Schema: `06_workflow_schema.json`
- 실행기: `05_workflow_engine.rs`
- Step 타입: `memory_recall`, `memory_store`, `sql`, `llm`, `tool_call`, `file_write`, `calendar_add`, `phone_action`, `shell`, `conditional`, `loop`, `user_confirm`
- 변수 치환: `{{input.x}}`, `{{step_id.output}}` (Handlebars-like)
- 비용 상한 필수: `max_tokens_per_run`, `max_llm_calls_per_run`

### D-3. 생성 경로 (Voice → YAML)

```
① 음성 입력 "…자동으로 해줘"
    ↓ 로컬 STT
② Intent Classifier (온디바이스 SLM)
    분류: create_workflow / run_workflow / edit_workflow / delete_workflow / other
    ↓
③ Scaffolder (Claude Opus)
    context 구성:
      - 사용자 발화
      - 모든 등록된 도구 목록 (tool_registry)
      - RRF 로 유사 워크플로우 3개 검색 → few-shot 예시
      - 온톨로지에서 관련 Object 샘플
      - 사용자의 기본 카테고리 선호도
    출력: YAML 초안
    ↓
④ Validator
    - JSON Schema 검증
    - 도구 권한 검증
    - 비용 상한 삽입 (없으면 기본값 주입)
    - Dry-run: 각 step mock 실행
    ↓
⑤ 사용자 확인 UI
    카드: [제목/트리거/단계 N개/예상 비용/예상 소요시간]
    사용자 선택: [저장] / [수정(음성)] / [취소]
    수정 시 → Scaffolder 가 기존 YAML + diff 요청 처리 → 패치
    ↓
⑥ 저장
    - workflows INSERT
    - FTS 인덱스 업데이트
    - 델타 저널 기록
    - 사용자에게 음성 피드백: "저장했어요. ○○ 카테고리에서 확인하세요."
```

### D-4. 실행 경로 (Trigger → Run)

### 트리거 타입

- `voice`: 핫워드 ("모아, 상담일지 작성") → FTS 매칭 → 실행
- `schedule`: cron 표현식 → `src/scheduler/` 가 기동
- `event`: 시스템 이벤트 (`phone_call_ended`, `memory_stored`, `calendar_event_added`)
- `manual`: 카테고리 화면에서 탭

### 실행 흐름

```
Trigger → workflow_engine.exec(workflow, inputs)
  ├─ workflow_runs INSERT (status=running, input_sha256)
  ├─ steps 순차 실행 (또는 parallel 블록)
  ├─ 각 step 결과를 context 에 누적
  ├─ user_confirm step 발생 시 일시정지 + 알림
  ├─ 오류 시: rollback 가능한 step 은 롤백, 아니면 status=failed
  └─ 완료: status=success, output_sha256, cost 기록
```

### D-5. 학습 루프 (Dream Cycle 통합)

Dream Cycle 작업 #5 의 상세 정의:

```
FOR each workflow WHERE usage_count >= 5:
    runs = workflow_runs WHERE workflow_id = w.id LIMIT 50 ORDER BY started_at DESC

    # 실패율 분석
    IF failed(runs) / count(runs) > 0.2:
        call LLM to analyze failure patterns → suggest patch
        → insert into user's inbox as "워크플로우 개선 제안"

    # 반복 파라미터 감지
    param_values = extract inputs from runs
    FOR each input param:
        IF mode(param_values).frequency > 0.7:
            → suggest default value update

    # 추상화 제안
    similar_workflows = find by embedding similarity > 0.85, count >= 3
    IF similar_workflows:
        → suggest merging into parameterized higher-level workflow
```

---

## Part E. 변호사 프리셋 10종 (초기 탑재)

모두 `resources/workflow_presets/*.yaml` 로 제공. 첫 실행 시 자동 import.

1. **신규 의뢰인 접수 인터뷰** (전화비서) — 전화 수신 → 이름/연락처/사건유형 청취 → 온톨로지 Object 생성 → 사건 일정 템플릿 생성
2. **상담일지 자동 작성** (문서작업) — 의뢰인 이름 입력 → 최근 통화/메모 RRF 검색 → Opus 초안 → Gemini 리뷰 → Opus 최종본 → .docx 저장
3. **판례 3단 요약** (문서작업) — 판례 URL/텍스트 → 쟁점/판단/시사점 3섹션 구조화
4. **소장/답변서 초안** (문서작업) — 사건 기본정보 → 관련 판례 검색 → 6단계 mixture-of-agents 초안
5. **계약서 리뷰** (문서작업) — PDF 업로드 → 조항별 리스크 체크리스트 → 수정 제안
6. **기일 전 브리핑 자동 생성** (일상업무, 매일 08:00) — 오늘 기일 조회 → 각 사건 compiled_truth + 최근 이벤트 → 카드형 브리핑
7. **의뢰인별 주간 진행 리포트** (일상업무, 매주 금 17:00) — 의뢰인 전체 순회 → 주간 진행 요약 → 발송
8. **법정 녹음 통역** (통역) — 외국인 의뢰인 동석 시 실시간 통역 + 전사본 저장
9. **사건 타임라인 자동 업데이트** (전화비서+문서 복합) — 새 통화 종료 이벤트 → 해당 사건 타임라인에 자동 append
10. **영수증/비용 정리** (일상업무, OCR) — 영수증 사진 → 항목 추출 → 사건별 비용 집계

---

## Part F. 구현 로드맵 & 완료 기준

00_CLAUDE_CODE_INSTRUCTIONS.md §3 참조. 스프린트별 Definition of Done 는 §6 참조.

---

### 끝. 본 문서는 Claude Code 가 구현 중 모호한 부분을 판단할 때 기준이 된다.

