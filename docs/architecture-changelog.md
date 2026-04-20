# MoA Architecture — Change Log

> This file records version-by-version changes and session summaries that used
> to live at the top of `docs/ARCHITECTURE.md`. The main architecture document
> now focuses on the current design only; historical deltas are preserved here
> for auditability.

---

## v7.0 — 2026-04-18 (SLM-First Gatekeeper + Advisor Strategy + On-device Executor)

**Headline**: Gemma 4 SLM 1차 응답 + 이용자 최고사양 LLM 을 PLAN/REVIEW/ADVISE advisor 로 호출 + Phase 3 SLM-as-executor tool loop.

This session rewired the core chat pipeline around three principles:
(1) on-device SLM (Gemma 4) is the first responder to every post-login
message, (2) when SLM defers, an advisor-class LLM (the user's top-tier
model) is consulted at 3 regular checkpoints, and (3) tool-using tasks
run on the SLM's own prompt-guided tool loop with cloud LLM fallback.

**Entry points** (in main ARCHITECTURE.md):
- §"MoA Core Workflow" rewritten around SLM-first routing.
- §"Advisor Strategy" subsection describes PLAN/REVIEW/ADVISE checkpoints, category-based policy, and 2.2× key routing.
- §"Phase 2" / §"Phase 3" subsections cover revision loop, WS wiring, smart_search cascade, PLAN suggested_tools, and the SLM-as-executor loop (prompt-guided XML tool-call protocol).

**Shipped tracks (commits on `feat/document-pipeline-overhaul`)**:

| Track | Commit | Summary |
| --- | --- | --- |
| bin compile fix | `fef995fa` | `main.rs` 에 `mod local_llm` 추가 — 4 개 E0433 복구 (SLM-first 기능이 shipped 바이너리에서 실제 코드 경로로 실행되지 않던 버그) |
| SLM-first REST | `312ac26a` | `GatekeeperRouter` 를 `AppState.gatekeeper` 로 wire, `host_probe` 가 Gemma 4 티어 자동 선정, `/api/chat` 은 SLM 분류 → Local 이면 Ollama 즉답 / Cloud 면 agent loop fall-through |
| SkillForge 제거 | `be189e07` | 1,122 LOC 고아 모듈 삭제 (CLI/API/스케줄러 어디에서도 호출 안 됨) |
| SLM-first WS | `cb865990` | `/ws/chat` 에 동일 SLM-first 배선 |
| dual-compile 가드 | `902d9b32` | `main.rs` / `lib.rs` 간 모듈 symmetry 회귀를 CI에서 잡는 `tests/dual_compile_symmetry.rs`, lib-only 모듈 5 개 mirror |
| Phase 1 Advisor | `22c1535e` | `AdvisorClient::{plan,review,advise}`, `AdvisorPolicy::for_category`, `top_tier_model_for` (provider → Opus/GPT/Gemini 최고사양 매핑), AppState 연결, REST `/api/chat` PLAN+REVIEW wiring |
| Phase 2 | `e9b63785` | 자동 revision 루프 (`RevisionNeeded → 1 재실행 → 재리뷰`) · `/ws/chat` advisor · `smart_search` cascade (무료 → Perplexity → 4 회 재조합) · `PlanOutput.suggested_tools` |
| Phase 3 executor | `c11e86af` | `SlmExecutor` (프롬프트-가이드 XML tool loop, max 8 iters, cloud fallback) — Medium / tool_hint task 를 Gemma 4 가 직접 실행, 실패 시 cloud LLM 으로 자동 폴백 |
| safe_for_slm 큐레이션 | `3e817a73` | `Tool::safe_for_slm()` trait 메서드; `shell` / `delegate` / `apply_patch` / `file_write` / `file_edit` / `cron_*` 는 `false` override → SLM executor 에는 안 넘김, cloud LLM agent loop 는 변경 없음 |

**Validation** (HEAD `3e817a73`):

- `cargo check --all-targets` — 0 errors
- `cargo test --lib` — **5,716 passed / 0 failed / 10 ignored** (v6.1 baseline 5,586 → +130 new tests across advisor + slm_executor + smart_search + symmetry guard)
- `cargo test --test dual_compile_symmetry` — 2 / 0

**Phase 3 의도적 비포함**:

- SLM 스트리밍 (XML 태그 경계 버퍼링 + WS 통합 필요 — 별도 PR)
- 멀티 모델 SLM executor (T3/T4 Gemma 티어 자동 선택 — 현재는 단일 모델)

### Doc sync sweep (2026-04-17, post-code-verify)

- §6 High-Level Module Map refreshed to match current `src/lib.rs` (previous map omitted `approval/`, `auth/`, `bin/`, `categories/`, `coordination/`, `cost/`, `cron/`, `daemon/`, `desktop/`, `dispatch/`, `doctor/`, `economic/`, `goals/`, `hardware/`, `health/`, `heartbeat/`, `hooks/`, `integrations/`, `onboard/`, `phone/`, `rag/`, `service/`, `services/`, `session_search/`, `skillforge/`, `skills/`, `storage/`, `tunnel/`, `user_model/`, `vault/`, `workflow/`, and the root files `identity.rs`, `migration.rs`, `multimodal.rs`, `update.rs`, `util.rs`).
- §6F-10 Item #3 (`SqliteMemory::apply_remote_v3_delta` fallthrough) marked ✅ DONE — dispatch for `SkillUpsert` / `UserProfileConclusion` / `CorrectionPatternUpsert` shipped in `24c7009c` (2026-04-16) and lives at `src/memory/sqlite.rs:2340–2409` with forwarding tests `apply_remote_v3_delta_forwards_{skill_upsert,user_profile_conclusion,correction_pattern}` (lines 4945/4968/4990).
- Items #1 (Agent Loop `SessionHandle` wire), #2 (channel session start/end), and #4 (Tauri UI) of §6F-10 re-verified as still pending: no references to `SessionHandle`, `procedural::`, `user_model::`, or `session_search::` in `src/agent/loop_.rs` as of HEAD `fd0b42fa`.

---

## v6.1 — 2026-04-16 (Self-Learning Skill System)

- §6F **Self-Learning Skill System** (new): Hermes Agent 레포(NousResearch/hermes-agent) 분석 후 MoA에 없는 3가지 핵심 기능을 접목 — 자기 생성 스킬 시스템 (procedural memory), 사용자 행동 모델링 (cross-session profiling), 세션 검색 (FTS5 대화 원문 recall). 추가로 기획 단계에서 도출된 **자기 학습형 교정 스킬**(이용자 수정 행동 관찰 → 검증 → 패턴화 → 추천 → 피드백 5단계 파이프라인)을 문서 카테고리의 첫 구체 구현체로 포함. 22개 신규 파일, ~3,400 LOC, **166 신규 단위 테스트 전체 통과**. 기존 동기화 엔진(Patent 1)·Dream Cycle·도구 레지스트리와 매끄럽게 통합.
- `DeltaOperation` enum에 `SkillUpsert` / `UserProfileConclusion` / `CorrectionPatternUpsert` 3개 변형 추가 → 멀티디바이스 동기화 자동 확장.
- Dream Cycle에 Task 7 추가 (저사용 스킬 아카이브 + 프로파일 confidence decay + 교정 패턴 decay).
- §11 특허 혁신 영역에 **Patent 5 후보** 등록 (이용자 편집 행위 관찰 기반 자기 개선 교정).

---

## v6.0 — 2026-04-15 (Dual-Brain Second Memory + Vault)

- §3b **Patent 3 — Dual-Brain Second Memory** (new): compiled_truth + append-only timeline + Dream Cycle.
- §3c **Sync Journal v3** (new): `TimelineAppend`, `PhoneCallRecord`, `CompiledTruthUpdate` delta operations + inbound `apply_remote_v3_delta` hook with LWW on `truth_version`.
- §6★★ updated: hybrid search now defaults to weighted but `search_mode = "rrf"` unlocks Reciprocal Rank Fusion (k=60) via `Memory::recall` and `Memory::recall_with_variations` for multi-query expansion.
- **§6D MoA Vault (Second Brain)** (new, **production complete**): full implementation of v6 plan — 7-step wikilink extraction pipeline, 17 vault tables (incl. `vault_embeddings`), self-evolving vocabulary, `VaultDocUpsert` sync delta, unified first+second brain parallel search with mandatory `chat_retrieval_logs` audit, **4-way RAG** (FTS + vector + graph BFS + meta filter) with `QueryKind` adaptive weights, production **hub note engine** (4 entity skeletons + priority queue + 3-tier conflict resolution + Light/Heavy/Full Rebuild incremental updates + Evidence Gap warnings), vault health scoring (0–100), **7-section focus briefing** with LLM narrative + incremental cache, three pluggable AI engines (`HeuristicAIEngine` / `LlmAIEngine` cloud / `OllamaSlmEngine` on-device HTTP), `Converter` trait with CLI backend (pandoc / pdftotext / hwp5html), polling `FolderWatcher` with dual-format (.md + .html) artifact persistence, **idle-time `VaultScheduler`** orchestrating hub compile + health + briefing refresh. Patent 4 claims 23–29 formalised here. **109 vault tests + 0 regressions** (506 total pass). R-series follow-ups shipped: LLM-based hub section assignment (`compile_hub_with_ai`), parallel compile worker (`compile_batch`, tokio Semaphore), semantic tag clustering (`semantic_tag_clusters`, EmbeddingProvider-backed), LLM contradiction detection (`detect_entity_contradictions` → `hub_notes.conflict_pending`).
- §3 §6★★ §6A already existed; only wording/integration points refreshed to match `src/memory/*` as of commit `831d070e`.
- **§6E Plan↔Code traceability matrix** (new): single-source verification that every planned item (brain-v3 S1-S9 / vault-v6 §1-§11 / multi-device sync §8-10 / quantitative+qualitative ingest gate) is implemented, with file:line cites + 513 passing tests.

---

## Prior history

For earlier session summaries (2026-03-xx and before) see git log:

```
git log --follow docs/ARCHITECTURE.md
```
