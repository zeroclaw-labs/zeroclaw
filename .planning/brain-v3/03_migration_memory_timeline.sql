-- ============================================================
-- MoA v3.0 — Memory Timeline + Phone + Workflow Migration
-- File: migrations/20260414_v3_timeline.sql
-- DB: SQLite (with sqlite-vec + FTS5)
--
-- ⚠️  Claude Code 주의사항:
--  1. 이 마이그레이션은 기존 memories.content 를 절대 삭제하지 않는다.
--  2. 실행 전 WAL 체크포인트 + 백업 필수.
--  3. sync 모듈의 델타 저널 대상 테이블 목록에 아래 신규 테이블들을 추가해야 함
--     (src/sync/journal.rs 의 JOURNALED_TABLES const).
-- ============================================================

BEGIN TRANSACTION;

-- ──────────────────────────────────────────────────────────
-- 1. memories 테이블 확장 (비파괴)
-- ──────────────────────────────────────────────────────────
ALTER TABLE memories ADD COLUMN compiled_truth TEXT;
ALTER TABLE memories ADD COLUMN truth_version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE memories ADD COLUMN truth_updated_at INTEGER;
ALTER TABLE memories ADD COLUMN needs_recompile INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_memories_needs_recompile
    ON memories(needs_recompile) WHERE needs_recompile = 1;

-- ──────────────────────────────────────────────────────────
-- 2. memory_timeline — append-only 증거 저장소
-- ──────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS memory_timeline (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    uuid            TEXT NOT NULL UNIQUE,                    -- 동기화 ID
    memory_id       INTEGER NOT NULL,
    event_type      TEXT NOT NULL CHECK(event_type IN (
                        'call','chat','doc','manual','workflow','email','ocr'
                    )),
    event_at        INTEGER NOT NULL,                        -- unix ts (ms)
    source_ref      TEXT NOT NULL,                           -- call_uuid / msg_id / file_sha256 (NOT NULL — 감사)
    content         TEXT NOT NULL,                           -- 원본 증거 (수정 금지)
    content_sha256  TEXT NOT NULL,                           -- 무결성 검증용
    metadata_json   TEXT,                                    -- {gps, duration, 발신번호, ...}
    device_id       TEXT NOT NULL,                           -- 어느 기기에서 생성되었는가
    created_at      INTEGER NOT NULL DEFAULT (unixepoch()),
    FOREIGN KEY (memory_id) REFERENCES memories(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_timeline_memory_time
    ON memory_timeline(memory_id, event_at DESC);
CREATE INDEX IF NOT EXISTS idx_timeline_source_ref
    ON memory_timeline(source_ref);
CREATE INDEX IF NOT EXISTS idx_timeline_event_type
    ON memory_timeline(event_type, event_at DESC);

-- 업데이트 방지 트리거 (append-only 보장)
CREATE TRIGGER IF NOT EXISTS trg_timeline_no_update
BEFORE UPDATE ON memory_timeline
BEGIN
    SELECT RAISE(ABORT, 'memory_timeline is append-only');
END;

-- FTS5 미러 (자연어 검색)
CREATE VIRTUAL TABLE IF NOT EXISTS memory_timeline_fts
    USING fts5(content, source_ref UNINDEXED, memory_id UNINDEXED,
               content='memory_timeline', content_rowid='id');

CREATE TRIGGER IF NOT EXISTS trg_timeline_ai AFTER INSERT ON memory_timeline BEGIN
    INSERT INTO memory_timeline_fts(rowid, content, source_ref, memory_id)
    VALUES (new.id, new.content, new.source_ref, new.memory_id);
END;

-- ──────────────────────────────────────────────────────────
-- 3. phone_calls — 전화비서 통화 메타
-- ──────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS phone_calls (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    call_uuid           TEXT NOT NULL UNIQUE,
    direction           TEXT NOT NULL CHECK(direction IN ('in','out','missed')),
    caller_number       TEXT,
    caller_number_e164  TEXT,
    caller_object_id    INTEGER,                             -- 온톨로지 매칭 결과
    started_at          INTEGER NOT NULL,
    ended_at            INTEGER,
    duration_ms         INTEGER,
    gps_lat             REAL,
    gps_lon             REAL,
    transcript          TEXT,
    summary             TEXT,
    risk_level          TEXT CHECK(risk_level IN ('safe','warn','danger')) DEFAULT 'safe',
    sos_triggered       INTEGER NOT NULL DEFAULT 0,
    language            TEXT,                                -- 자동 감지 ko/en/...
    memory_id           INTEGER,                             -- timeline 진입점
    device_id           TEXT NOT NULL,
    created_at          INTEGER NOT NULL DEFAULT (unixepoch()),
    FOREIGN KEY (caller_object_id) REFERENCES ontology_objects(id) ON DELETE SET NULL,
    FOREIGN KEY (memory_id) REFERENCES memories(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_phone_calls_number
    ON phone_calls(caller_number_e164, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_phone_calls_object
    ON phone_calls(caller_object_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_phone_calls_risk
    ON phone_calls(risk_level, started_at DESC) WHERE risk_level != 'safe';

-- ──────────────────────────────────────────────────────────
-- 4. user_categories — 사용자 Custom 카테고리
-- ──────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS user_categories (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    uuid            TEXT NOT NULL UNIQUE,
    name            TEXT NOT NULL,
    icon            TEXT,
    parent_seed_key TEXT,                                    -- 'document' 등 또는 NULL (최상위)
    order_index     INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at      INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE(name, parent_seed_key)
);

CREATE INDEX IF NOT EXISTS idx_user_categories_order
    ON user_categories(parent_seed_key, order_index);

-- ──────────────────────────────────────────────────────────
-- 5. workflows — 파생 워크플로우
-- ──────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS workflows (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    uuid                 TEXT NOT NULL UNIQUE,
    parent_category      TEXT NOT NULL,                      -- seed_key 또는 user_category.uuid
    name                 TEXT NOT NULL,
    description          TEXT,
    icon                 TEXT,
    spec_yaml            TEXT NOT NULL,                      -- YAML DSL 본문
    spec_sha256          TEXT NOT NULL,                      -- 변조 감지
    trigger_type         TEXT NOT NULL CHECK(trigger_type IN (
                              'voice','schedule','event','manual'
                         )),
    trigger_config_json  TEXT,
    version              INTEGER NOT NULL DEFAULT 1,
    parent_workflow_id   INTEGER,                            -- fork / 이전 버전 참조
    created_by           TEXT NOT NULL CHECK(created_by IN (
                              'user','ai_generated','preset','imported'
                         )),
    usage_count          INTEGER NOT NULL DEFAULT 0,
    last_used_at         INTEGER,
    is_pinned            INTEGER NOT NULL DEFAULT 0,
    is_archived          INTEGER NOT NULL DEFAULT 0,
    created_at           INTEGER NOT NULL DEFAULT (unixepoch()),
    updated_at           INTEGER NOT NULL DEFAULT (unixepoch()),
    FOREIGN KEY (parent_workflow_id) REFERENCES workflows(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_wf_category
    ON workflows(parent_category, is_pinned DESC, usage_count DESC)
    WHERE is_archived = 0;

CREATE VIRTUAL TABLE IF NOT EXISTS workflows_fts
    USING fts5(name, description, content='workflows', content_rowid='id');

CREATE TRIGGER IF NOT EXISTS trg_wf_ai AFTER INSERT ON workflows BEGIN
    INSERT INTO workflows_fts(rowid, name, description)
    VALUES (new.id, new.name, new.description);
END;
CREATE TRIGGER IF NOT EXISTS trg_wf_ad AFTER DELETE ON workflows BEGIN
    INSERT INTO workflows_fts(workflows_fts, rowid, name, description)
    VALUES('delete', old.id, old.name, old.description);
END;
CREATE TRIGGER IF NOT EXISTS trg_wf_au AFTER UPDATE ON workflows BEGIN
    INSERT INTO workflows_fts(workflows_fts, rowid, name, description)
    VALUES('delete', old.id, old.name, old.description);
    INSERT INTO workflows_fts(rowid, name, description)
    VALUES (new.id, new.name, new.description);
END;

-- ──────────────────────────────────────────────────────────
-- 6. workflow_runs — 실행 이력 (감사 + 학습)
-- ──────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS workflow_runs (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    uuid              TEXT NOT NULL UNIQUE,
    workflow_id       INTEGER NOT NULL,
    workflow_version  INTEGER NOT NULL,
    started_at        INTEGER NOT NULL,
    ended_at          INTEGER,
    status            TEXT NOT NULL CHECK(status IN (
                          'running','success','failed','cancelled','paused'
                      )),
    trigger_source    TEXT,                                  -- 'voice'/'schedule'/'manual'/'event:phone_call_ended'
    input_json        TEXT,
    input_sha256      TEXT,                                  -- 법적 감사
    output_ref        TEXT,                                  -- 생성 파일 경로 / memory_id 등
    output_sha256     TEXT,
    error_message     TEXT,
    cost_tokens_in    INTEGER DEFAULT 0,
    cost_tokens_out   INTEGER DEFAULT 0,
    cost_llm_calls    INTEGER DEFAULT 0,
    feedback_rating   INTEGER,                               -- 1~5 (선택)
    feedback_note     TEXT,
    device_id         TEXT NOT NULL,
    FOREIGN KEY (workflow_id) REFERENCES workflows(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_wfruns_workflow
    ON workflow_runs(workflow_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_wfruns_status
    ON workflow_runs(status, started_at DESC);

-- ──────────────────────────────────────────────────────────
-- 7. workflow_suggestions — Dream Cycle 산출물 (개선 제안 인박스)
-- ──────────────────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS workflow_suggestions (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    uuid             TEXT NOT NULL UNIQUE,
    workflow_id      INTEGER,                                -- NULL 가능 (새 워크플로우 제안)
    suggestion_type  TEXT NOT NULL CHECK(suggestion_type IN (
                         'fix_failure','default_value','abstraction','deprecation'
                     )),
    title            TEXT NOT NULL,
    description      TEXT NOT NULL,
    patch_yaml       TEXT,                                   -- 제안되는 YAML 패치
    created_at       INTEGER NOT NULL DEFAULT (unixepoch()),
    reviewed_at      INTEGER,
    review_decision  TEXT CHECK(review_decision IN ('accepted','rejected','snoozed')),
    FOREIGN KEY (workflow_id) REFERENCES workflows(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_wfsug_pending
    ON workflow_suggestions(created_at DESC) WHERE reviewed_at IS NULL;

-- ──────────────────────────────────────────────────────────
-- 8. 스키마 버전 기록
-- ──────────────────────────────────────────────────────────
INSERT INTO schema_migrations (version, applied_at, description)
VALUES ('20260414_v3_timeline', unixepoch(),
        'v3.0: memory_timeline, phone_calls, user_categories, workflows, workflow_runs, workflow_suggestions');

COMMIT;

-- ============================================================
-- 적용 후 필수 후속 작업 (애플리케이션 코드):
--  1. src/sync/journal.rs 의 JOURNALED_TABLES 에 아래 추가:
--       "memory_timeline", "phone_calls", "user_categories",
--       "workflows", "workflow_runs", "workflow_suggestions"
--  2. src/memory/repo.rs 에 compiled_truth / timeline 접근 메서드 추가
--  3. 기존 통합 테스트 재실행 — memories.content 기반 쿼리 모두 정상 동작해야 함
-- ============================================================
