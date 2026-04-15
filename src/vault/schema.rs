// @Ref: SUMMARY §2 — all v6 tables + idempotent migrations.
//
// Runs against the same brain.db used by first brain (SqliteMemory).
// Additive only: never ALTER or DROP existing memories/phone_calls/
// ontology tables. Each CREATE is IF NOT EXISTS — safe to call at
// every process start.

use rusqlite::Connection;

/// Initialise all vault tables + FTS5 mirror + triggers.
///
/// Called from `VaultStore::with_shared_connection` at construction.
/// Safe to re-run: every statement is idempotent.
pub fn init_schema(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        r#"
        -- ── §2.1 Core document store ────────────────────────────
        CREATE TABLE IF NOT EXISTS vault_documents (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            uuid             TEXT NOT NULL UNIQUE,
            title            TEXT,
            content          TEXT NOT NULL,
            html_content     TEXT,
            source_type      TEXT NOT NULL CHECK(source_type IN
                                ('local_file','chat_upload','chat_paste','remote')),
            source_device_id TEXT,
            original_path    TEXT,
            checksum         TEXT NOT NULL UNIQUE,
            doc_type         TEXT,
            char_count       INTEGER NOT NULL DEFAULT 0,
            created_at       INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at       INTEGER NOT NULL DEFAULT (unixepoch()),
            -- Embedding metadata (PR #2: model lock-in prevention)
            embedding_model    TEXT,        -- e.g. "bge-m3"
            embedding_dim      INTEGER,     -- 1024 for BGE-M3
            embedding_provider TEXT,        -- "local_fastembed" / "openai" / "custom_http"
            embedding_version  INTEGER NOT NULL DEFAULT 1,
            embedding_created_at INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_vault_docs_emb_model
            ON vault_documents(embedding_model) WHERE embedding_model IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_vault_docs_source
            ON vault_documents(source_type, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_vault_docs_device
            ON vault_documents(source_device_id, created_at DESC);

        -- ── §2.1b Vault embeddings (kept separate to keep vault_documents lean) ──
        CREATE TABLE IF NOT EXISTS vault_embeddings (
            doc_id     INTEGER PRIMARY KEY,
            embedding  BLOB NOT NULL,
            dim        INTEGER NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            FOREIGN KEY (doc_id) REFERENCES vault_documents(id) ON DELETE CASCADE
        );

        -- ── §2.2 Wikilinks + backlinks ───────────────────────────
        CREATE TABLE IF NOT EXISTS vault_links (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            source_doc_id  INTEGER NOT NULL,
            target_raw     TEXT NOT NULL,
            target_doc_id  INTEGER,
            display_text   TEXT,
            link_type      TEXT NOT NULL DEFAULT 'wikilink'
                            CHECK(link_type IN ('wikilink','alias','embed','block')),
            context        TEXT,
            line_number    INTEGER,
            is_resolved    INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY (source_doc_id) REFERENCES vault_documents(id) ON DELETE CASCADE,
            FOREIGN KEY (target_doc_id) REFERENCES vault_documents(id) ON DELETE SET NULL
        );
        CREATE INDEX IF NOT EXISTS idx_vault_links_source
            ON vault_links(source_doc_id);
        CREATE INDEX IF NOT EXISTS idx_vault_links_target_raw
            ON vault_links(target_raw);
        CREATE INDEX IF NOT EXISTS idx_vault_links_target_id
            ON vault_links(target_doc_id) WHERE target_doc_id IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_vault_links_unresolved
            ON vault_links(target_raw) WHERE is_resolved = 0;

        -- ── §2.3 Tags / aliases / frontmatter / blocks ───────────
        CREATE TABLE IF NOT EXISTS vault_tags (
            id       INTEGER PRIMARY KEY AUTOINCREMENT,
            doc_id   INTEGER NOT NULL,
            tag_name TEXT NOT NULL,
            tag_type TEXT,
            UNIQUE(doc_id, tag_name),
            FOREIGN KEY (doc_id) REFERENCES vault_documents(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_vault_tags_name ON vault_tags(tag_name);

        CREATE TABLE IF NOT EXISTS vault_aliases (
            id     INTEGER PRIMARY KEY AUTOINCREMENT,
            doc_id INTEGER NOT NULL,
            alias  TEXT NOT NULL UNIQUE,
            FOREIGN KEY (doc_id) REFERENCES vault_documents(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS vault_frontmatter (
            id     INTEGER PRIMARY KEY AUTOINCREMENT,
            doc_id INTEGER NOT NULL,
            key    TEXT NOT NULL,
            value  TEXT,
            UNIQUE(doc_id, key),
            FOREIGN KEY (doc_id) REFERENCES vault_documents(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_vault_fm_key ON vault_frontmatter(key, value);

        CREATE TABLE IF NOT EXISTS vault_blocks (
            id       INTEGER PRIMARY KEY AUTOINCREMENT,
            doc_id   INTEGER NOT NULL,
            block_id TEXT NOT NULL,
            content  TEXT NOT NULL,
            UNIQUE(doc_id, block_id),
            FOREIGN KEY (doc_id) REFERENCES vault_documents(id) ON DELETE CASCADE
        );

        -- ── §2.4 FTS5 mirror (distinct from memories_fts) ────────
        -- PR #3: trigram tokenizer for Korean — unicode61 default misses
        -- morphology ("김대리가" vs "김대리"). trigram matches 3-char windows
        -- so conjugation/particle variation is recallable.
        CREATE VIRTUAL TABLE IF NOT EXISTS vault_docs_fts USING fts5(
            title,
            content,
            content='vault_documents',
            content_rowid='id',
            tokenize='trigram'
        );
        CREATE TRIGGER IF NOT EXISTS vault_docs_ai
        AFTER INSERT ON vault_documents BEGIN
            INSERT INTO vault_docs_fts(rowid, title, content)
            VALUES (new.id, new.title, new.content);
        END;
        CREATE TRIGGER IF NOT EXISTS vault_docs_ad
        AFTER DELETE ON vault_documents BEGIN
            INSERT INTO vault_docs_fts(vault_docs_fts, rowid, title, content)
            VALUES ('delete', old.id, old.title, old.content);
        END;
        CREATE TRIGGER IF NOT EXISTS vault_docs_au
        AFTER UPDATE ON vault_documents BEGIN
            INSERT INTO vault_docs_fts(vault_docs_fts, rowid, title, content)
            VALUES ('delete', old.id, old.title, old.content);
            INSERT INTO vault_docs_fts(rowid, title, content)
            VALUES (new.id, new.title, new.content);
        END;

        -- ── §2.5 Self-evolving vocabulary (P1: co_pairs + relations; P3 quality) ─
        CREATE TABLE IF NOT EXISTS co_pairs (
            word_a TEXT NOT NULL,
            word_b TEXT NOT NULL,
            count  INTEGER NOT NULL DEFAULT 1,
            PRIMARY KEY (word_a, word_b)
        );

        CREATE TABLE IF NOT EXISTS vocabulary_relations (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            word_a         TEXT NOT NULL,
            word_b         TEXT NOT NULL,
            relation_type  TEXT NOT NULL CHECK(relation_type IN
                            ('synonym','near_synonym','antonym','hypernym','related')),
            representative TEXT,
            confidence     REAL NOT NULL DEFAULT 0.5,
            source_count   INTEGER NOT NULL DEFAULT 1,
            domain         TEXT,
            created_at     INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at     INTEGER NOT NULL DEFAULT (unixepoch()),
            UNIQUE(word_a, word_b, relation_type)
        );
        CREATE INDEX IF NOT EXISTS idx_vocab_rel_rep
            ON vocabulary_relations(representative) WHERE representative IS NOT NULL;

        CREATE TABLE IF NOT EXISTS boilerplate_words (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            word           TEXT NOT NULL UNIQUE,
            domain         TEXT,
            frequency_rank TEXT,
            reason         TEXT,
            created_at     INTEGER NOT NULL DEFAULT (unixepoch())
        );

        -- ── §2.6 Phase 2 schema stubs (engines built later) ──────
        CREATE TABLE IF NOT EXISTS hub_notes (
            id                 INTEGER PRIMARY KEY AUTOINCREMENT,
            entity_name        TEXT NOT NULL UNIQUE,
            hub_subtype        TEXT,
            backlink_count     INTEGER NOT NULL DEFAULT 0,
            importance_score   REAL NOT NULL DEFAULT 0,
            hub_threshold      INTEGER NOT NULL DEFAULT 5,
            compile_type       TEXT,
            last_compiled      INTEGER,
            pending_backlinks  INTEGER NOT NULL DEFAULT 0,
            structure_elements INTEGER NOT NULL DEFAULT 0,
            mapped_documents   INTEGER NOT NULL DEFAULT 0,
            conflict_resolved  INTEGER NOT NULL DEFAULT 0,
            conflict_pending   INTEGER NOT NULL DEFAULT 0,
            content_md         TEXT
        );

        CREATE TABLE IF NOT EXISTS co_occurrences (
            entity_a TEXT NOT NULL,
            entity_b TEXT NOT NULL,
            count    INTEGER NOT NULL DEFAULT 1,
            PRIMARY KEY (entity_a, entity_b)
        );

        -- ── §2.7 Phase 4 schema stubs ────────────────────────────
        CREATE TABLE IF NOT EXISTS health_reports (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            report_date      INTEGER NOT NULL DEFAULT (unixepoch()),
            health_score     REAL NOT NULL,
            orphan_count     INTEGER NOT NULL DEFAULT 0,
            unresolved_count INTEGER NOT NULL DEFAULT 0,
            conflict_count   INTEGER NOT NULL DEFAULT 0,
            report_md        TEXT
        );

        CREATE TABLE IF NOT EXISTS briefings (
            id             INTEGER PRIMARY KEY AUTOINCREMENT,
            case_number    TEXT NOT NULL UNIQUE,
            created_at     INTEGER NOT NULL DEFAULT (unixepoch()),
            last_updated   INTEGER NOT NULL DEFAULT (unixepoch()),
            status         TEXT NOT NULL DEFAULT 'active'
                            CHECK(status IN ('active','archived')),
            briefing_path  TEXT
        );

        -- ── §2.8 Cross-brain registry + retrieval logs ───────────
        CREATE TABLE IF NOT EXISTS entity_registry (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            canonical_name      TEXT NOT NULL UNIQUE,
            entity_type         TEXT,
            first_brain_ref     TEXT,
            second_brain_doc_id INTEGER,
            created_at          INTEGER NOT NULL DEFAULT (unixepoch()),
            FOREIGN KEY (second_brain_doc_id)
                REFERENCES vault_documents(id) ON DELETE SET NULL
        );

        CREATE TABLE IF NOT EXISTS chat_retrieval_logs (
            id                INTEGER PRIMARY KEY AUTOINCREMENT,
            chat_msg_id       TEXT,
            query             TEXT NOT NULL,
            first_brain_hits  INTEGER NOT NULL DEFAULT 0,
            second_brain_hits INTEGER NOT NULL DEFAULT 0,
            latency_ms        INTEGER NOT NULL DEFAULT 0,
            merged_top_refs   TEXT,
            created_at        INTEGER NOT NULL DEFAULT (unixepoch())
        );
        CREATE INDEX IF NOT EXISTS idx_retrieval_created
            ON chat_retrieval_logs(created_at DESC);
        "#,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn mem_conn() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn init_schema_idempotent() {
        let conn = mem_conn();
        init_schema(&conn).unwrap();
        init_schema(&conn).unwrap(); // re-run
        init_schema(&conn).unwrap(); // third run
    }

    #[test]
    fn all_v6_tables_present() {
        let conn = mem_conn();
        init_schema(&conn).unwrap();
        let expected = [
            "vault_documents",
            "vault_links",
            "vault_tags",
            "vault_aliases",
            "vault_frontmatter",
            "vault_blocks",
            "vault_docs_fts",
            "co_pairs",
            "vocabulary_relations",
            "boilerplate_words",
            "hub_notes",
            "co_occurrences",
            "health_reports",
            "briefings",
            "entity_registry",
            "chat_retrieval_logs",
        ];
        for name in expected {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE name = ?1 AND type IN ('table','virtual')",
                    [name],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(count >= 1, "missing table: {name}");
        }
    }

    #[test]
    fn fts_trigger_mirrors_inserts() {
        let conn = mem_conn();
        init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO vault_documents (uuid, title, content, source_type, checksum, char_count)
             VALUES ('u1','민법 제750조 해설','[[민법 제750조]]는 불법행위의 일반 조항이다','chat_paste','c1',30)",
            [],
        )
        .unwrap();
        let hits: i64 = conn
            .query_row(
                // trigram tokenizer requires ≥3-char contiguous substring
                "SELECT COUNT(*) FROM vault_docs_fts WHERE vault_docs_fts MATCH '불법행'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }
}
