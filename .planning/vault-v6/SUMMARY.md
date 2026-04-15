---
name: MoA Vault v6 — Second Brain Implementation Spec
description: Terse single-source-of-truth guide that the src/vault/ module must match. Every commit cites a section from this file.
type: project
---

# MoA Vault v6 — Second Brain Build Guide

> **목적**: 광범위한 기획안 전체를 읽지 않고도 구현이 방향을 잃지 않게 하는 **코드-우선 요약**.
> 모든 `src/vault/**` 파일은 이 문서의 §번호를 `// @Ref: §x-y`로 인용해야 한다.

---

## §1. Scope & Phase Gating

| Phase | 범위 | 상태 목표 |
|---|---|---|
| **P1 (이번 세션)** | 스키마, 인제스트, 7단계 위키링크 파이프라인, 통합검색 훅, vault sync delta | **작동 + 테스트** |
| P2 | 허브노트 엔진 (뼈대+구조매핑+적응형갱신) | 스키마만 존재, 구현 미착수 |
| P3 | 자기진화 어휘사전 고도화, AI 게이트키퍼 LLM 품질 업그레이드, 복합토큰 확장 | 기본 버전만 |
| P4 | 헬스체크, 포커스 브리핑 | 스키마만 |
| P5 | 파일 watcher, 다디바이스 vault sync end-to-end, 옵시디언 호환 | 훅만 |

**이번 세션 DoD**: 긴 법률 텍스트를 `vault::ingest_markdown`에 넣으면 (a) 복합 토큰이 식별되고 (b) 핵심 키워드에 `[[]]`가 본문 삽입되며 (c) `vault_links`에 row가 생기고 (d) `unified_search`가 퍼스트+세컨드 브레인을 **병렬**로 조회한다. `cargo test vault::` 전체 green.

---

## §2. 데이터베이스 (SQLite, 기존 `brain.db` 공유)

기존 `memories`·`phone_calls`·온톨로지 테이블은 **불변**. 아래는 v6 **추가** 테이블:

```sql
vault_documents(
  id INTEGER PK, uuid TEXT UNIQUE, title TEXT, content TEXT, html_content TEXT,
  source_type TEXT CHECK(source_type IN ('local_file','chat_upload','chat_paste')),
  source_device_id TEXT, original_path TEXT, checksum TEXT UNIQUE,
  doc_type TEXT, char_count INTEGER, created_at INTEGER, updated_at INTEGER
)
vault_links(
  id INTEGER PK, source_doc_id INTEGER, target_raw TEXT, target_doc_id INTEGER NULL,
  display_text TEXT, link_type TEXT DEFAULT 'wikilink',
  context TEXT, line_number INTEGER, is_resolved INTEGER DEFAULT 0
)
vault_tags(id INTEGER PK, doc_id INTEGER, tag_name TEXT, tag_type TEXT)
vault_aliases(id INTEGER PK, doc_id INTEGER, alias TEXT UNIQUE)
vault_frontmatter(id INTEGER PK, doc_id INTEGER, key TEXT, value TEXT)
vault_blocks(id INTEGER PK, doc_id INTEGER, block_id TEXT, content TEXT, UNIQUE(doc_id,block_id))
vault_docs_fts USING fts5(title, content, content='vault_documents', content_rowid='id')
co_pairs(word_a TEXT, word_b TEXT, count INTEGER DEFAULT 1, PRIMARY KEY(word_a,word_b))
vocabulary_relations(
  id INTEGER PK, word_a TEXT, word_b TEXT,
  relation_type TEXT CHECK(relation_type IN ('synonym','near_synonym','antonym','hypernym','related')),
  representative TEXT, confidence REAL DEFAULT 0.5, source_count INTEGER DEFAULT 1,
  domain TEXT, created_at INTEGER, updated_at INTEGER,
  UNIQUE(word_a, word_b, relation_type)
)
boilerplate_words(id INTEGER PK, word TEXT UNIQUE, domain TEXT, frequency_rank TEXT, reason TEXT, created_at INTEGER)
-- Phase 2~4 스키마만 선생성 (엔진 구현은 해당 phase에서)
hub_notes(id INTEGER PK, entity_name TEXT UNIQUE, hub_subtype TEXT, backlink_count INTEGER DEFAULT 0,
  importance_score REAL DEFAULT 0, compile_type TEXT, last_compiled INTEGER NULL,
  pending_backlinks INTEGER DEFAULT 0, structure_elements INTEGER DEFAULT 0,
  mapped_documents INTEGER DEFAULT 0, conflict_resolved INTEGER DEFAULT 0,
  conflict_pending INTEGER DEFAULT 0, content_md TEXT)
health_reports(id INTEGER PK, report_date INTEGER, health_score REAL,
  orphan_count INTEGER, unresolved_count INTEGER, conflict_count INTEGER, report_md TEXT)
briefings(id INTEGER PK, case_number TEXT UNIQUE, created_at INTEGER, last_updated INTEGER,
  status TEXT DEFAULT 'active', briefing_path TEXT)
entity_registry(id INTEGER PK, canonical_name TEXT UNIQUE, entity_type TEXT,
  first_brain_ref TEXT, second_brain_doc_id INTEGER)
chat_retrieval_logs(id INTEGER PK, chat_msg_id TEXT, query TEXT,
  first_brain_hits INTEGER, second_brain_hits INTEGER, latency_ms INTEGER,
  merged_top_refs TEXT, created_at INTEGER)
```

모든 테이블 **idempotent** (`CREATE TABLE IF NOT EXISTS`). `vault_documents`에 `AFTER INSERT/UPDATE/DELETE` 트리거로 FTS5 미러 유지.

---

## §3. 7단계 위키링크 파이프라인

모듈: `src/vault/wikilink/`

| Step | 파일 | 입력 | 출력 | AI? |
|---|---|---|---|---|
| 0. 복합 토큰 | `tokens.rs` | markdown text | `Vec<CompoundToken { kind, span, canonical }>` | ❌ regex |
| 1. 정량 점수 | `frequency.rs` | markdown + 복합 토큰 + synonym map | `HashMap<term, f32>` (TF+heading 가산) | ❌ |
| 2a. AI 정성 | `ai.rs::extract_key_concepts` | markdown + 복합 토큰 | `Vec<(term, importance 1~10)>` | ✅ via `AIEngine` |
| 2b. 상용구 필터 | `boilerplate.rs` | 후보 리스트 + boilerplate_words | 필터링된 리스트 | ❌ DB 조회 |
| 3. 교차 검증 | `cross_validate.rs` | 축1·축2 결과 | 확정 후보 (≤상한) | ❌ 규칙 |
| 4. AI 게이트키퍼 | `ai.rs::gatekeep` | 확정 후보 + markdown 스니펫 | 최종 키워드 + 동의어 쌍 | ✅ |
| 5. 삽입 | `insert.rs` | markdown + 키워드 + aliases | `[[]]`/`[[rep\|alias]]` 삽입된 markdown + `Vec<LinkRecord>` | ❌ |
| 6. 어휘 학습 | `vocabulary.rs` | 확정 키워드 집합 | co_pairs upsert, synonym→vocabulary_relations | ❌ |

### §3.1 복합 토큰 regex (한국어 법률, P1 범위)
- 판례번호: `대법원 YYYY\. ?M{1,2}\. ?D{1,2}\. 선고 YYYY다\d+ 판결` (variants: 선고, 자, 결정)
- 사건번호: `\d{4}(가합|가단|가소|고합|고단|구합|구단|재가합)\d+`
- 법조문: `(민법|형법|상법|민사소송법|형사소송법|행정소송법) 제\d+조(의\d+)?( 제\d+항)?`
- 기관명: `㈜[\w가-힣]+`, `법무법인(\(유한\))?\s*[\w가-힣]+`

### §3.2 분량 상한
- ≤500자: 5개, ≤2000자: 10개, ≤5000자: 15개, >5000자: 20개

### §3.3 AIEngine trait (§6 의존성)
```rust
pub trait AIEngine: Send + Sync {
    async fn extract_key_concepts(&self, markdown: &str, compounds: &[CompoundToken])
        -> Result<Vec<KeyConcept>>;
    async fn gatekeep(&self, candidates: &[String], doc_preview: &str)
        -> Result<GatekeepVerdict>; // kept, synonym_pairs
}
```
P1 기본 구현체: `HeuristicAIEngine` (provider 없이도 작동하는 규칙 기반) + `LlmAIEngine` (Haiku 호출). 테스트는 Heuristic 사용.

---

## §4. 인제스트 경로

```
source_type ∈ {local_file, chat_upload, chat_paste}
  chat_paste: content.len() >= DOCUMENT_MIN_CHARS (2000) 아닐 시 스킵
  │
  ▼ VaultStore::ingest_markdown(meta, content)
      1. checksum 계산; 동일 checksum 존재 → Ok(existing_id) 반환 (멱등)
      2. frontmatter 파싱 (YAML front-matter, key=value 추출)
      3. 위키링크 파이프라인 실행 → content', Vec<LinkRecord>, Vec<KeyConcept>
      4. vault_documents INSERT
      5. vault_frontmatter, vault_tags, vault_aliases, vault_links INSERT
      6. FTS5 트리거가 자동 색인
      7. sync engine attached이면 record_vault_doc_upsert delta push
      8. return vault_doc_id
```

---

## §5. 통합 검색 (§9 of plan)

모듈: `src/vault/unified_search.rs`

```rust
pub async fn unified_search(
    memory: &dyn Memory,            // first brain
    vault: &VaultStore,             // second brain
    query: &str,
    session_id: Option<&str>,
    scope: SearchScope,             // Both | FirstOnly | SecondOnly
    top_k: usize,
) -> Vec<UnifiedHit>
```

- 퍼스트 검색 (`memory.recall`) 과 세컨드 검색 (`vault.search_fts + search_vector`) 을 **`tokio::join!`로 병렬**.
- 개별 300ms 타임아웃 → 부분 결과 허용.
- RRF 병합 (k=60) 후 top_k 반환.
- `chat_retrieval_logs`에 first/second hit 수, latency, merged refs **항상 기록**.
- `UnifiedHit { source: First|Second, ref_id, snippet, score, ranker_trace }`.

---

## §6. Sync 통합

기존 `DeltaOperation`에 **추가** (불변):
```rust
DeltaOperation::VaultDocUpsert {
    uuid: String,
    source_type: String,
    title: Option<String>,
    checksum: String,
    content_sha256: String,
    frontmatter_json: Option<String>,
    links_json: Option<String>, // LinkRecord 배열 직렬화
}
```

- Outbound: `VaultStore::ingest_markdown` 성공 시 `record_vault_doc_upsert` 자동 호출.
- Inbound: `Memory::apply_remote_v3_delta`에 매치. SqliteMemory override가 `vault_documents INSERT OR IGNORE` + links 재삽입 (`uuid` 유니크로 idempotent).
- 기존 Patent 1 E2E 암호화 파이프라인이 그대로 운반 → 추가 네트워크 코드 **불필요**.

---

## §7. 모듈 레이아웃

```
src/vault/
├── mod.rs               — re-exports, AIEngine trait, DOCUMENT_MIN_CHARS
├── schema.rs            — init_schema(&Connection) -> Result<()>
├── store.rs             — VaultStore struct, ingest_markdown, search_*, attach_sync
├── ingest.rs            — IngestInput, IngestOutput, pipeline orchestration
├── wikilink/
│   ├── mod.rs           — WikilinkPipeline coordinator
│   ├── tokens.rs        — CompoundToken, regex (Step 0)
│   ├── frequency.rs     — Step 1
│   ├── ai.rs            — AIEngine trait + HeuristicAIEngine + LlmAIEngine (Step 2a, 4)
│   ├── boilerplate.rs   — Step 2b
│   ├── cross_validate.rs— Step 3
│   ├── insert.rs        — Step 5
│   └── vocabulary.rs    — Step 6 (co_pairs, vocabulary_relations updates)
├── unified_search.rs    — parallel first+second brain search + RRF
└── tests.rs             — E2E integration tests
```

---

## §8. 원칙

1. **확장은 additive**: 기존 `memories`, `phone_calls`, sync journal 스키마 훼손 금지.
2. **모든 Step은 trait 뒤에 배치**: Step 2a/4는 `AIEngine`, Step 1은 함수로 충분.
3. **Provider 의존 금지**: P1 기본 엔진은 `HeuristicAIEngine` — 테스트/오프라인 환경에서 작동.
4. **멱등**: `checksum` 유니크 + `UUID` 유니크로 중복 인제스트 차단.
5. **fail-closed**: Step 4 gatekeep 실패 시 빈 키워드로 fallback (링크 없는 문서로 저장 후 Dream Cycle에서 재처리).
6. **테스트 커버리지**: Step 0 regex, Step 3 상한, Step 5 삽입, E2E ingest, unified_search 병렬성 — 최소 15개.

---

## §9. 이번 세션 체크리스트

- [ ] `.planning/vault-v6/SUMMARY.md` ← 본 문서
- [ ] `src/vault/schema.rs` 16개 테이블 idempotent create
- [ ] `src/vault/wikilink/{tokens,frequency,boilerplate,cross_validate,insert,vocabulary}.rs`
- [ ] `src/vault/wikilink/ai.rs` — AIEngine trait + HeuristicAIEngine
- [ ] `src/vault/store.rs` — VaultStore + ingest + search_fts + search_vector + attach_sync
- [ ] `src/vault/unified_search.rs` — parallel + RRF + chat_retrieval_logs 기록
- [ ] `src/memory/sync.rs` — VaultDocUpsert variant + record_vault_doc_upsert
- [ ] `src/memory/sqlite.rs::apply_remote_v3_delta` — VaultDocUpsert 처리
- [ ] 15+ 테스트 green
- [ ] `docs/ARCHITECTURE.md` §6D 완성

끝.
