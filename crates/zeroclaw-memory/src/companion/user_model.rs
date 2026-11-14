//! Local-first User Model authority: owner values, goals,
//! preferences as governed, append-only records — not an inferred profile
//! and not a generic memcore category.
//!
//! Authority rules (frozen in the User Model spec):
//! - An explicit owner-authored statement may become an active revision
//!   immediately.
//! - An observation is ALWAYS a candidate; no amount of repetition
//!   promotes it. Only an explicit review action (`accept`/`narrow`) can.
//! - `reject` records the decision without deleting evidence.
//! - `supersede` appends a new revision; history is never rewritten.
//! - Works fully offline; nothing here requires Tachi.

use std::fmt;
use std::fmt::Write as _;

use super::user_model_scope::{ApplicabilityContext, Scope};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use rusqlite::Connection;
use zeroclaw_infra::sqlite_perms::harden_sqlite_owner_only;

/// The shared User Model reflection queue is bounded independently of Soul.
pub const USER_MODEL_MAX_OPEN_REFLECTION_CANDIDATES: usize = 3;
pub const USER_MODEL_STATEMENT_MAX_BYTES: usize = 240;

pub fn validate_review_text(text: &str) -> Result<(), rusqlite::Error> {
    if text.trim().is_empty()
        || text.len() > USER_MODEL_STATEMENT_MAX_BYTES
        || text.chars().any(char::is_control)
    {
        return Err(rusqlite::Error::InvalidParameterName(
            "statement must be a non-empty single line of at most 240 bytes".into(),
        ));
    }
    Ok(())
}

fn is_owner_correction_key(key: &str) -> bool {
    key.strip_prefix("oc.").is_some_and(|digest| {
        digest.len() == 60
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// U5 corrections use an agent/session-local namespace, while historical
/// semantic-key supersession remains unchanged for every other producer.
fn owner_correction_key(agent: &str, session: &str, semantic_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"owner-correction\0");
    for part in [agent, session, semantic_key] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    let digest = format!("{:x}", digest.finalize());
    format!("oc.{}", &digest[..60])
}

/// Resolve provenance from the immutable candidate, never from revision prose
/// or a duplicated agent column. Malformed reserved-namespace rows project nowhere.
fn correction_evidence_matches(evidence: &str, agent: &str, session: &str, key: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(evidence) else {
        return false;
    };
    let Some(submitted_key) = value["submitted_semantic_key"].as_str() else {
        return false;
    };
    let Some(messages) = value["messages"].as_array() else {
        return false;
    };
    if value["origin"] != "owner_correction"
        || value["agent"] != agent
        || agent.trim().is_empty()
        || agent.trim() != agent
        || session.trim().is_empty()
        || session.trim() != session
        || session.chars().any(char::is_control)
        || messages.len() != 1
        || submitted_key.is_empty()
        || submitted_key.len() > 64
        || !submitted_key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        || owner_correction_key(agent, session, submitted_key) != key
    {
        return false;
    }
    let message = &messages[0];
    message["session_id"] == session
        && message["at_unix"].as_u64().is_some()
        && message["owner_text"]
            .as_str()
            .is_some_and(|text| !text.trim().is_empty())
        && serde_json::from_value::<zeroclaw_api::review::UserMessageSource>(
            message["source"].clone(),
        )
        .is_ok()
}

/// What kind of statement this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserModelKind {
    Value,
    Goal,
    Preference,
    Habit,
    Constraint,
}

impl UserModelKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Goal => "goal",
            Self::Preference => "preference",
            Self::Habit => "habit",
            Self::Constraint => "constraint",
        }
    }
}

/// How a revision earned authority. Frequency and confidence never appear
/// here — evidence quality is not authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityClass {
    OwnerAuthored,
    OwnerRatified,
}

impl AuthorityClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::OwnerAuthored => "owner_authored",
            Self::OwnerRatified => "owner_ratified",
        }
    }
}

/// The review actions an owner can take on a candidate. Each produces a
/// distinct append-only history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAction {
    Accept,
    Reject,
    Narrow,
    Supersede,
}

/// A committed review already decides this candidate. The one supported
/// follow-up is narrowing a rejected candidate.
#[derive(Debug)]
struct CandidateAlreadyReviewed;

impl fmt::Display for CandidateAlreadyReviewed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("candidate already reviewed")
    }
}

impl std::error::Error for CandidateAlreadyReviewed {}

/// Classify the store's typed review conflict without matching error text.
pub fn is_candidate_already_reviewed(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::ToSqlConversionFailure(inner) if inner.is::<CandidateAlreadyReviewed>())
}

/// Typed approval conflict: the current eligible head differs from the one shown.
#[derive(Debug)]
struct HeadConflict;
impl fmt::Display for HeadConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("head_conflict")
    }
}
impl std::error::Error for HeadConflict {}

pub fn is_user_model_head_conflict(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::ToSqlConversionFailure(inner) if inner.is::<HeadConflict>())
}

impl ReviewAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Reject => "reject",
            Self::Narrow => "narrow",
            Self::Supersede => "supersede",
        }
    }
}

/// An observation awaiting review. Never active on its own, regardless of
/// how many times it (or its siblings) was observed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UserModelCandidate {
    pub id: String,
    pub kind: UserModelKind,
    pub statement: String,
    pub semantic_key: String,
    pub scope: String,
    pub evidence: String,
    pub created_at_unix: u64,
}

impl UserModelCandidate {
    pub(crate) fn visible_to_agent(&self, agent: &str) -> bool {
        if !is_owner_correction_key(&self.semantic_key) || self.scope == "global" {
            return true;
        }
        let Some(Scope::Session(session)) = Scope::parse(&self.scope) else {
            return false;
        };
        self.scope == Scope::Session(session.clone()).to_string()
            && correction_evidence_matches(&self.evidence, agent, &session, &self.semantic_key)
    }
}

/// An append-only revision. The active head for a semantic key is derived
/// at read time (latest applicable revision), so supersession never edits
/// history.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UserModelRevision {
    pub id: String,
    pub semantic_key: String,
    pub kind: UserModelKind,
    pub statement: String,
    pub scope: String,
    pub authority: AuthorityClass,
    pub supersedes: Option<String>,
    pub valid_from_unix: u64,
    pub valid_until_unix: Option<u64>,
    pub source_candidate: Option<String>,
    pub created_at_unix: u64,
}

/// Receipt for an explicit review decision. Even a rejection keeps the
/// candidate and its evidence intact.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UserModelReviewReceipt {
    pub id: String,
    pub candidate_id: String,
    pub action: ReviewAction,
    pub reviewer: String,
    pub note: Option<String>,
    pub at_unix: u64,
}

/// Append-only sqlite store for the User Model (`user_model.db` under the
/// companion data dir; owner-only, WAL).
pub struct UserModelStore {
    conn: Mutex<Connection>,
}

const OWNER_STATEMENT_SUPERSEDES_SQL: &str = "SELECT id FROM user_model_revisions
                 WHERE semantic_key = ?1 AND valid_from_unix <= ?2
                 ORDER BY created_at_unix DESC, id DESC LIMIT 1";

impl UserModelStore {
    /// Open (or create) the store. Blocking; call outside async contexts
    /// or via `spawn_blocking`.
    pub fn open(data_dir: &Path) -> Result<Self, rusqlite::Error> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let db_path = data_dir.join("user_model.db");
        create_owner_only_file(&db_path)?;
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             CREATE TABLE IF NOT EXISTS user_model_candidates (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                statement TEXT NOT NULL,
                semantic_key TEXT NOT NULL,
                scope TEXT NOT NULL DEFAULT 'global',
                evidence TEXT NOT NULL DEFAULT '[]',
                created_at_unix INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS user_model_revisions (
                id TEXT PRIMARY KEY,
                semantic_key TEXT NOT NULL,
                kind TEXT NOT NULL,
                statement TEXT NOT NULL,
                scope TEXT NOT NULL DEFAULT 'global',
                authority TEXT NOT NULL,
                supersedes TEXT,
                valid_from_unix INTEGER NOT NULL,
                valid_until_unix INTEGER,
                source_candidate TEXT,
                created_at_unix INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS user_model_review_receipts (
                id TEXT PRIMARY KEY,
                candidate_id TEXT NOT NULL,
                action TEXT NOT NULL,
                reviewer TEXT NOT NULL,
                note TEXT,
                at_unix INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_user_model_revisions_key_created_valid
                ON user_model_revisions (semantic_key, created_at_unix, valid_from_unix);",
        )?;
        harden_sqlite_owner_only(&db_path);
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// One shared handle per data directory for the whole process, so the
    /// review API and every prompt projection read through the same
    /// connection. Blocking on first open.
    pub fn shared(data_dir: &Path) -> Result<Arc<Self>, rusqlite::Error> {
        static HANDLES: OnceLock<Mutex<HashMap<PathBuf, Arc<UserModelStore>>>> = OnceLock::new();
        let handles = HANDLES.get_or_init(|| Mutex::new(HashMap::new()));
        let mut handles = handles.lock();
        if let Some(store) = handles.get(data_dir) {
            return Ok(Arc::clone(store));
        }
        let store = Arc::new(Self::open(data_dir)?);
        handles.insert(data_dir.to_path_buf(), Arc::clone(&store));
        Ok(store)
    }

    /// Record an explicit owner-authored statement and make it the active
    /// revision for its semantic key immediately (local-first; no review
    /// round-trip required when the owner speaks directly).
    ///
    /// Lookup and insert share ONE lock scope so two concurrent writers on
    /// the same key cannot both supersede the same prior revision.
    pub fn record_owner_statement(
        &self,
        kind: UserModelKind,
        statement: &str,
        semantic_key: &str,
        scope: &str,
        now_unix: u64,
    ) -> Result<UserModelRevision, rusqlite::Error> {
        if Scope::parse(scope).is_none() {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "invalid scope '{scope}': global | agent:<id> | channel:<id> | session:<id>"
            )));
        }
        let conn = self.conn.lock();
        let supersedes: Option<String> = conn
            .query_row(
                OWNER_STATEMENT_SUPERSEDES_SQL,
                rusqlite::params![semantic_key, now_unix],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        let revision = UserModelRevision {
            id: uuid::Uuid::new_v4().to_string(),
            semantic_key: semantic_key.to_string(),
            kind,
            statement: statement.to_string(),
            scope: scope.to_string(),
            authority: AuthorityClass::OwnerAuthored,
            supersedes,
            valid_from_unix: now_unix,
            valid_until_unix: None,
            source_candidate: None,
            created_at_unix: now_unix,
        };
        conn.execute(
            "INSERT INTO user_model_revisions
                 (id, semantic_key, kind, statement, scope, authority, supersedes,
                  valid_from_unix, valid_until_unix, source_candidate, created_at_unix)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                revision.id,
                revision.semantic_key,
                revision.kind.as_str(),
                revision.statement,
                revision.scope,
                revision.authority.as_str(),
                revision.supersedes,
                revision.valid_from_unix,
                revision.valid_until_unix,
                revision.source_candidate,
                revision.created_at_unix,
            ],
        )?;
        Ok(revision)
    }

    /// Record an observation as a candidate. Repeated observations only
    /// add evidence; there is deliberately no API that promotes a
    /// candidate by count, frequency, or confidence.
    pub fn record_observation(
        &self,
        kind: UserModelKind,
        statement: &str,
        semantic_key: &str,
        evidence: &str,
        now_unix: u64,
    ) -> Result<UserModelCandidate, rusqlite::Error> {
        self.insert_observation(
            kind,
            statement,
            semantic_key,
            evidence,
            now_unix,
            false,
            "global",
        )?
        .ok_or(rusqlite::Error::InvalidQuery)
    }

    /// Reflection observations remain pending. One transaction enforces the
    /// queue bound and suppresses identical pending observations across writers.
    pub fn record_reflection_observation(
        &self,
        kind: UserModelKind,
        statement: &str,
        semantic_key: &str,
        evidence: &str,
        now_unix: u64,
    ) -> Result<Option<UserModelCandidate>, rusqlite::Error> {
        validate_review_text(statement)?;
        if semantic_key.is_empty()
            || semantic_key.len() > 64
            || !semantic_key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(rusqlite::Error::InvalidParameterName(
                "invalid semantic key".into(),
            ));
        }
        self.insert_observation(
            kind,
            statement,
            semantic_key,
            evidence,
            now_unix,
            true,
            "global",
        )
    }

    /// A correction shares the bounded review queue, but applies only to its
    /// originating session if the owner later accepts it.
    #[allow(clippy::too_many_arguments)]
    pub fn record_owner_correction(
        &self,
        agent: &str,
        kind: UserModelKind,
        statement: &str,
        semantic_key: &str,
        evidence: &str,
        session: &str,
        now_unix: u64,
    ) -> Result<Option<UserModelCandidate>, rusqlite::Error> {
        validate_review_text(statement)?;
        if session.trim().is_empty()
            || session.trim() != session
            || session.chars().any(char::is_control)
            || semantic_key.is_empty()
            || semantic_key.len() > 64
            || !semantic_key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(rusqlite::Error::InvalidParameterName(
                "invalid correction scope or semantic key".into(),
            ));
        }
        let correction_key = owner_correction_key(agent, session, semantic_key);
        if !correction_evidence_matches(evidence, agent, session, &correction_key) {
            return Err(rusqlite::Error::InvalidParameterName(
                "invalid owner correction provenance".into(),
            ));
        }
        self.insert_observation(
            kind,
            statement,
            &correction_key,
            evidence,
            now_unix,
            true,
            &Scope::Session(session.to_string()).to_string(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_observation(
        &self,
        kind: UserModelKind,
        statement: &str,
        semantic_key: &str,
        evidence: &str,
        now_unix: u64,
        bounded: bool,
        scope: &str,
    ) -> Result<Option<UserModelCandidate>, rusqlite::Error> {
        let candidate = UserModelCandidate {
            id: uuid::Uuid::new_v4().to_string(),
            kind,
            statement: statement.to_string(),
            semantic_key: semantic_key.to_string(),
            scope: scope.to_string(),
            evidence: evidence.to_string(),
            created_at_unix: now_unix,
        };
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if bounded {
            let duplicate: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM user_model_candidates c WHERE kind = ?1 AND semantic_key = ?2 AND statement = ?3 AND scope = ?4 AND NOT EXISTS(SELECT 1 FROM user_model_review_receipts r WHERE r.candidate_id = c.id))", rusqlite::params![kind.as_str(), semantic_key, statement, scope], |r| r.get(0))?;
            let pending: usize = tx.query_row("SELECT COUNT(*) FROM user_model_candidates c WHERE NOT EXISTS(SELECT 1 FROM user_model_review_receipts r WHERE r.candidate_id = c.id)", [], |r| r.get(0))?;
            if duplicate || pending >= USER_MODEL_MAX_OPEN_REFLECTION_CANDIDATES {
                return Ok(None);
            }
        }
        tx.execute(
            "INSERT INTO user_model_candidates
                 (id, kind, statement, semantic_key, scope, evidence, created_at_unix)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                candidate.id,
                candidate.kind.as_str(),
                candidate.statement,
                candidate.semantic_key,
                candidate.scope,
                candidate.evidence,
                candidate.created_at_unix,
            ],
        )?;
        tx.commit()?;
        Ok(Some(candidate))
    }

    /// All candidates, newest first, with their evidence.
    pub fn list_candidates(&self) -> Result<Vec<UserModelCandidate>, rusqlite::Error> {
        self.list_candidates_by_review(false)
    }

    /// Candidates with no committed review receipt, in history order.
    pub fn list_pending_candidates(&self) -> Result<Vec<UserModelCandidate>, rusqlite::Error> {
        self.list_candidates_by_review(true)
    }

    fn list_candidates_by_review(
        &self,
        pending: bool,
    ) -> Result<Vec<UserModelCandidate>, rusqlite::Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, kind, statement, semantic_key, scope, evidence, created_at_unix
             FROM user_model_candidates c
             WHERE (?1 = 0 OR NOT EXISTS (
                 SELECT 1 FROM user_model_review_receipts r WHERE r.candidate_id = c.id
             ))
             ORDER BY created_at_unix DESC, id DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![pending], |row| {
            let kind_raw: String = row.get(1)?;
            Ok((UserModelCandidate {
                id: row.get(0)?,
                kind: kind_from_str(&kind_raw).ok_or(rusqlite::Error::QueryReturnedNoRows)?,
                statement: row.get(2)?,
                semantic_key: row.get(3)?,
                scope: row.get(4)?,
                evidence: row.get(5)?,
                created_at_unix: row.get::<_, i64>(6)?.max(0) as u64,
            },))
        })?;
        let mut candidates = Vec::new();
        for row in rows {
            candidates.push(row?.0);
        }
        Ok(candidates)
    }

    /// An opt-in history page, newest first. Only the limited ID selection
    /// participates in the metadata sort; evidence is read for at most
    /// `limit + 1` candidates. No history, authority or receipt is changed.
    pub fn list_candidates_page(
        &self,
        pending: bool,
        limit: usize,
        before: Option<(u64, &str)>,
    ) -> Result<(Vec<UserModelCandidate>, bool), rusqlite::Error> {
        if !(1..=200).contains(&limit) {
            return Err(rusqlite::Error::InvalidParameterName(
                "limit must be 1..200".into(),
            ));
        }
        let before_time = before
            .map(|(at, _)| i64::try_from(at).map_err(|_| rusqlite::Error::InvalidQuery))
            .transpose()?;
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.kind, c.statement, c.semantic_key, c.scope, c.evidence, c.created_at_unix
             FROM user_model_candidates c
             WHERE c.id IN (
                 SELECT p.id FROM user_model_candidates p
                 WHERE (?1 = 0 OR NOT EXISTS (
                     SELECT 1 FROM user_model_review_receipts r WHERE r.candidate_id = p.id
                 )) AND (?2 IS NULL OR p.created_at_unix < ?2
                     OR (p.created_at_unix = ?2 AND p.id < ?3))
                 ORDER BY p.created_at_unix DESC, p.id DESC LIMIT ?4
             )
             ORDER BY c.created_at_unix DESC, c.id DESC",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![
                pending,
                before_time,
                before.map(|(_, id)| id),
                (limit + 1) as i64
            ],
            |row| {
                let kind_raw: String = row.get(1)?;
                Ok(UserModelCandidate {
                    id: row.get(0)?,
                    kind: kind_from_str(&kind_raw).ok_or(rusqlite::Error::InvalidQuery)?,
                    statement: row.get(2)?,
                    semantic_key: row.get(3)?,
                    scope: row.get(4)?,
                    evidence: row.get(5)?,
                    created_at_unix: row.get::<_, i64>(6)?.max(0) as u64,
                })
            },
        )?;
        let mut candidates = rows.collect::<Result<Vec<_>, _>>()?;
        let has_more = candidates.len() > limit;
        candidates.truncate(limit);
        Ok((candidates, has_more))
    }

    /// One candidate and its committed review receipts in insertion order.
    /// The read transaction keeps the candidate and receipts on one
    /// committed snapshot, including after a store reopen.
    pub fn candidate_history(
        &self,
        candidate_id: &str,
    ) -> Result<Option<(UserModelCandidate, Vec<UserModelReviewReceipt>)>, rusqlite::Error> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let candidate = match tx.query_row(
            "SELECT id, kind, statement, semantic_key, scope, evidence, created_at_unix
             FROM user_model_candidates WHERE id = ?1",
            rusqlite::params![candidate_id],
            |row| {
                let kind_raw: String = row.get(1)?;
                Ok(UserModelCandidate {
                    id: row.get(0)?,
                    kind: kind_from_str(&kind_raw).ok_or(rusqlite::Error::InvalidQuery)?,
                    statement: row.get(2)?,
                    semantic_key: row.get(3)?,
                    scope: row.get(4)?,
                    evidence: row.get(5)?,
                    created_at_unix: row.get::<_, i64>(6)?.max(0) as u64,
                })
            },
        ) {
            Ok(candidate) => candidate,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(error) => return Err(error),
        };
        let receipts = {
            let mut stmt = tx.prepare(
                "SELECT id, candidate_id, action, reviewer, note, at_unix
                 FROM user_model_review_receipts
                 WHERE candidate_id = ?1 ORDER BY rowid ASC",
            )?;
            let rows = stmt.query_map(rusqlite::params![candidate_id], |row| {
                let action_raw: String = row.get(2)?;
                let action = match action_raw.as_str() {
                    "accept" => ReviewAction::Accept,
                    "reject" => ReviewAction::Reject,
                    "narrow" => ReviewAction::Narrow,
                    "supersede" => ReviewAction::Supersede,
                    _ => return Err(rusqlite::Error::InvalidQuery),
                };
                Ok(UserModelReviewReceipt {
                    id: row.get(0)?,
                    candidate_id: row.get(1)?,
                    action,
                    reviewer: row.get(3)?,
                    note: row.get(4)?,
                    at_unix: row.get::<_, i64>(5)?.max(0) as u64,
                })
            })?;
            let mut receipts = Vec::new();
            for row in rows {
                receipts.push(row?);
            }
            receipts
        };
        tx.commit()?;
        Ok(Some((candidate, receipts)))
    }

    /// Apply an eligible review action to a candidate. A successful action
    /// writes a receipt; `accept`/`narrow`/`supersede` additionally append a
    /// revision. `reject` never deletes the candidate or its evidence.
    pub fn review_candidate(
        &self,
        candidate_id: &str,
        action: ReviewAction,
        reviewer: &str,
        note: Option<&str>,
        narrowed_scope: Option<&str>,
        now_unix: u64,
    ) -> Result<UserModelReviewReceipt, rusqlite::Error> {
        self.review_candidate_with_text(
            candidate_id,
            action,
            reviewer,
            note,
            narrowed_scope,
            None,
            now_unix,
        )
    }

    /// Rewording is an explicit owner submission. The original candidate and
    /// evidence remain intact; the new revision owns the approved wording.
    #[allow(clippy::too_many_arguments)]
    pub fn review_candidate_with_text(
        &self,
        candidate_id: &str,
        action: ReviewAction,
        reviewer: &str,
        note: Option<&str>,
        narrowed_scope: Option<&str>,
        final_text: Option<&str>,
        now_unix: u64,
    ) -> Result<UserModelReviewReceipt, rusqlite::Error> {
        self.review_candidate_with_expected_head(
            candidate_id,
            action,
            reviewer,
            note,
            narrowed_scope,
            final_text,
            None,
            now_unix,
        )
    }

    /// Compare the exact displayed head under the same write transaction as
    /// the decision. Reject has no replacement and needs no head expectation.
    #[allow(clippy::too_many_arguments)]
    pub fn review_candidate_with_expected_head(
        &self,
        candidate_id: &str,
        action: ReviewAction,
        reviewer: &str,
        note: Option<&str>,
        narrowed_scope: Option<&str>,
        final_text: Option<&str>,
        expected_head: Option<&zeroclaw_api::companion::UserModelExpectedHead>,
        now_unix: u64,
    ) -> Result<UserModelReviewReceipt, rusqlite::Error> {
        if let Some(text) = final_text {
            if action == ReviewAction::Reject {
                return Err(rusqlite::Error::InvalidParameterName(
                    "reject does not accept final_text".into(),
                ));
            }
            validate_review_text(text)?;
        }
        let mut conn = self.conn.lock();
        // Acquire the database write slot before reading the decision so a
        // second store connection cannot act on the same pending snapshot.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let candidate = tx.query_row(
            "SELECT kind, statement, semantic_key, scope FROM user_model_candidates WHERE id = ?1",
            rusqlite::params![candidate_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )?;
        let kind =
            kind_from_str(&candidate.0).ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)?;

        let last_action = match tx.query_row(
            "SELECT action FROM user_model_review_receipts
             WHERE candidate_id = ?1 ORDER BY rowid DESC LIMIT 1",
            rusqlite::params![candidate_id],
            |row| row.get::<_, String>(0),
        ) {
            Ok(action) => Some(action),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(error) => return Err(error),
        };
        if let Some(last_action) = last_action {
            let prior = match last_action.as_str() {
                "accept" => ReviewAction::Accept,
                "reject" => ReviewAction::Reject,
                "narrow" => ReviewAction::Narrow,
                "supersede" => ReviewAction::Supersede,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            if prior != ReviewAction::Reject || action != ReviewAction::Narrow {
                // Preserve callers' rusqlite::Error contract while carrying
                // an exact domain marker. This is not a SQL conversion error.
                return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                    CandidateAlreadyReviewed,
                )));
            }
        }
        // Validate only actions that are still eligible. Reject ignores
        // narrowed_scope; the other actions retain their existing scope rules.
        let scope = match action {
            ReviewAction::Narrow => narrowed_scope.unwrap_or(&candidate.3),
            _ => &candidate.3,
        };
        if action != ReviewAction::Reject && Scope::parse(scope).is_none() {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "invalid narrowed scope '{scope}'"
            )));
        }
        if action != ReviewAction::Reject && candidate.3 != "global" && scope != candidate.3 {
            return Err(rusqlite::Error::InvalidParameterName(
                "review cannot widen or move candidate scope".into(),
            ));
        }
        let guarded_head = if action != ReviewAction::Reject {
            if let Some(expected) = expected_head {
                let current = active_heads_from_connection(&tx, now_unix, Some(&candidate.2))?
                    .into_iter()
                    .next()
                    .map(|head| head.id);
                if current != expected.id {
                    return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                        HeadConflict,
                    )));
                }
                Some(current)
            } else {
                None
            }
        } else {
            None
        };
        let receipt = UserModelReviewReceipt {
            id: uuid::Uuid::new_v4().to_string(),
            candidate_id: candidate_id.to_string(),
            action,
            reviewer: reviewer.to_string(),
            note: note.map(str::to_string),
            at_unix: now_unix,
        };
        tx.execute(
            "INSERT INTO user_model_review_receipts
                 (id, candidate_id, action, reviewer, note, at_unix)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                receipt.id,
                receipt.candidate_id,
                receipt.action.as_str(),
                receipt.reviewer,
                receipt.note,
                receipt.at_unix,
            ],
        )?;

        match action {
            ReviewAction::Reject => {}
            ReviewAction::Accept | ReviewAction::Narrow | ReviewAction::Supersede => {
                tx.execute(
                    "INSERT INTO user_model_revisions
                         (id, semantic_key, kind, statement, scope, authority, supersedes,
                          valid_from_unix, valid_until_unix, source_candidate, created_at_unix)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6,
                             CASE WHEN ?10 THEN ?11 ELSE (SELECT id FROM user_model_revisions
                              WHERE semantic_key = ?2
                                AND valid_from_unix <= ?7
                                AND (valid_until_unix IS NULL OR valid_until_unix > ?7)
                              ORDER BY created_at_unix DESC,
                                       CASE WHEN ?9 THEN rowid ELSE NULL END DESC,
                                       id DESC LIMIT 1) END,
                             ?7, NULL, ?8, ?7)",
                    rusqlite::params![
                        uuid::Uuid::new_v4().to_string(),
                        candidate.2,
                        kind.as_str(),
                        final_text.unwrap_or(&candidate.1),
                        scope,
                        AuthorityClass::OwnerRatified.as_str(),
                        now_unix,
                        candidate_id,
                        is_owner_correction_key(&candidate.2)
                            && matches!(Scope::parse(&candidate.3), Some(Scope::Session(_))),
                        guarded_head.is_some(),
                        guarded_head.as_ref().and_then(|head| head.as_deref()),
                    ],
                )?;
            }
        }
        tx.commit()?;
        Ok(receipt)
    }

    /// Active heads for an agent's prompt/reflection. Only the U5 session-candidate
    /// correction namespace requires agent provenance; other heads preserve
    /// the historical global/session applicability contract.
    pub fn active_heads_for_agent(
        &self,
        agent: &str,
        as_of_unix: Option<u64>,
    ) -> Result<Vec<UserModelRevision>, rusqlite::Error> {
        let heads = self.active_heads(as_of_unix)?;
        let conn = self.conn.lock();
        let mut admitted = Vec::new();
        for head in heads {
            if !is_owner_correction_key(&head.semantic_key) {
                admitted.push(head);
                continue;
            }
            let Some(candidate) = head.source_candidate.as_deref() else {
                // Historical owner statements have no candidate provenance.
                admitted.push(head);
                continue;
            };
            let provenance = conn.query_row(
                "SELECT semantic_key, scope, evidence FROM user_model_candidates WHERE id = ?1",
                [candidate],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            );
            let (key, scope, evidence) = match provenance {
                Ok(value) => value,
                Err(rusqlite::Error::QueryReturnedNoRows) => continue,
                Err(error) => return Err(error),
            };
            // Pre-U5 producers created global candidates, including candidates
            // later narrowed to a session. Their keys were never reserved.
            if scope == "global" {
                admitted.push(head);
                continue;
            }
            let Some(Scope::Session(session)) = Scope::parse(&scope) else {
                continue;
            };
            if key == head.semantic_key
                && scope == head.scope
                && scope == Scope::Session(session.clone()).to_string()
                && correction_evidence_matches(&evidence, agent, &session, &key)
            {
                admitted.push(head);
            }
        }
        Ok(admitted)
    }

    /// Active, applicable revisions as of `as_of_unix` (`None` = now).
    /// Per semantic key the latest revision valid at that instant wins;
    /// superseded and expired revisions are simply older history.
    pub fn active_heads(
        &self,
        as_of_unix: Option<u64>,
    ) -> Result<Vec<UserModelRevision>, rusqlite::Error> {
        let as_of = match as_of_unix {
            Some(as_of) => as_of,
            None => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?
                    .as_secs();
                // Validate SQLite's signed range without changing explicit as-of semantics.
                i64::try_from(now)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
                now
            }
        };
        let conn = self.conn.lock();
        active_heads_from_connection(&conn, as_of, None)
    }
}

const ACTIVE_HEADS_SQL: &str =
    "SELECT id, semantic_key, kind, statement, scope, authority, supersedes,
                    valid_from_unix, valid_until_unix, source_candidate, created_at_unix
             FROM user_model_revisions r
             WHERE r.valid_from_unix <= ?1
               AND (?2 IS NULL OR r.semantic_key = ?2)
               AND r.created_at_unix = (
                   SELECT MAX(r2.created_at_unix) FROM user_model_revisions r2
                   WHERE r2.semantic_key = r.semantic_key
                     AND r2.valid_from_unix <= ?1
               )
               AND NOT EXISTS (
                   SELECT 1 FROM user_model_candidates c
                   WHERE c.id = r.source_candidate
                     AND c.scope LIKE 'session:%'
                     AND length(c.semantic_key) = 63
                     AND substr(c.semantic_key, 1, 3) = 'oc.'
                     AND substr(c.semantic_key, 4) NOT GLOB '*[^0-9a-f]*'
                     AND EXISTS (
                         SELECT 1 FROM user_model_revisions newer
                         WHERE newer.semantic_key = r.semantic_key
                           AND newer.created_at_unix = r.created_at_unix
                           AND newer.valid_from_unix <= ?1
                           AND newer.rowid > r.rowid
                     )
               )
               AND (r.valid_until_unix IS NULL OR r.valid_until_unix > ?1)
             ORDER BY r.created_at_unix DESC, r.id DESC";

// Shared by the public projection and guarded review inside its Immediate
// transaction: expiry and owner-correction tie ordering have one query.
fn active_heads_from_connection(
    conn: &rusqlite::Connection,
    as_of: u64,
    semantic_key: Option<&str>,
) -> Result<Vec<UserModelRevision>, rusqlite::Error> {
    // Per key: take the newest revision that had already started at the
    // read instant, THEN gate it on its own validity window. A key whose
    // newest revision expired goes inactive — an older superseded
    // revision must never resurface through the gap.
    let mut stmt = conn.prepare(ACTIVE_HEADS_SQL)?;
    let rows = stmt.query_map(rusqlite::params![as_of, semantic_key], revision_from_row)?;
    let mut seen = std::collections::HashSet::new();
    let mut heads = Vec::new();
    for row in rows {
        let revision = row?;
        if seen.insert(revision.semantic_key.clone()) {
            heads.push(revision);
        }
    }
    heads.sort_by(|a, b| a.semantic_key.cmp(&b.semantic_key));
    Ok(heads)
}

/// Default character budget for the projected prompt section. The
/// projection is bounded so a large active set can never crowd out the
/// rest of the system prompt.
pub const USER_MODEL_PROJECTION_DEFAULT_MAX_CHARS: usize = 1_200;

/// The rendered projection plus the revision ids behind it. The ids are
/// for internal correction paths (logging, review UI) — they are NOT
/// embedded in the prompt text the model sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserModelStateProjection {
    /// Ready-to-append prompt section; empty when nothing is active.
    pub prompt_section: String,
    /// Active revision ids backing the section, newest first.
    pub revision_ids: Vec<String>,
}

/// Render the active heads into a bounded prompt section. Newest heads get
/// the budget first; anything that no longer fits is elided with a count.
pub fn project_active_heads(
    heads: &[UserModelRevision],
    max_chars: usize,
) -> UserModelStateProjection {
    if heads.is_empty() {
        return UserModelStateProjection {
            prompt_section: String::new(),
            revision_ids: Vec::new(),
        };
    }
    let mut ordered: Vec<&UserModelRevision> = heads.iter().collect();
    ordered.sort_by(|a, b| {
        b.created_at_unix
            .cmp(&a.created_at_unix)
            .then(a.id.cmp(&b.id))
    });

    let mut section = String::from("## Owner profile (authoritative)\n");
    let mut included_ids = Vec::new();
    let mut elided = 0usize;
    for revision in ordered {
        let line = format!(
            "- {}: {} [scope: {}]\n",
            revision.kind.as_str(),
            revision.statement,
            revision.scope
        );
        if section.len() + line.len() > max_chars {
            elided += 1;
            continue;
        }
        section.push_str(&line);
        included_ids.push(revision.id.clone());
    }
    if elided > 0 {
        let _ = writeln!(section, "- (+{elided} elided; review to trim)");
    }
    UserModelStateProjection {
        prompt_section: section,
        revision_ids: included_ids,
    }
}

/// Project the heads that apply in `applicability` (agent, channel,
/// session), bounded by `max_chars`. The single rendering of the owner
/// profile for every turn surface.
pub fn project_applicable_heads(
    heads: Vec<UserModelRevision>,
    applicability: &ApplicabilityContext,
    max_chars: usize,
) -> UserModelStateProjection {
    let applicable: Vec<_> = heads
        .into_iter()
        .filter(|revision| applicability.applies_str(&revision.scope))
        .collect();
    project_active_heads(&applicable, max_chars)
}

fn kind_from_str(raw: &str) -> Option<UserModelKind> {
    match raw {
        "value" => Some(UserModelKind::Value),
        "goal" => Some(UserModelKind::Goal),
        "preference" => Some(UserModelKind::Preference),
        "habit" => Some(UserModelKind::Habit),
        "constraint" => Some(UserModelKind::Constraint),
        _ => None,
    }
}

fn authority_from_str(raw: &str) -> Option<AuthorityClass> {
    match raw {
        "owner_authored" => Some(AuthorityClass::OwnerAuthored),
        "owner_ratified" => Some(AuthorityClass::OwnerRatified),
        _ => None,
    }
}

fn revision_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<UserModelRevision> {
    let kind_raw: String = row.get(2)?;
    let authority_raw: String = row.get(5)?;
    Ok(UserModelRevision {
        id: row.get(0)?,
        semantic_key: row.get(1)?,
        kind: kind_from_str(&kind_raw).ok_or(rusqlite::Error::QueryReturnedNoRows)?,
        statement: row.get(3)?,
        scope: row.get(4)?,
        authority: authority_from_str(&authority_raw)
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?,
        supersedes: row.get(6)?,
        valid_from_unix: row.get::<_, i64>(7)?.max(0) as u64,
        valid_until_unix: row.get::<_, Option<i64>>(8)?.map(|v| v.max(0) as u64),
        source_candidate: row.get(9)?,
        created_at_unix: row.get::<_, i64>(10)?.max(0) as u64,
    })
}

pub(super) fn create_owner_only_file(path: &Path) -> Result<(), rusqlite::Error> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    harden_sqlite_owner_only(path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, UserModelStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = UserModelStore::open(dir.path()).unwrap();
        (dir, s)
    }

    /// Discrimination 1: an explicit owner statement becomes active
    /// immediately, with no Tachi and no review round-trip.
    #[test]
    fn owner_statement_becomes_active_immediately() {
        let (_dir, s) = store();
        let revision = s
            .record_owner_statement(
                UserModelKind::Preference,
                "Always give me the engineering conclusion first.",
                "communication.conclusion-first",
                "global",
                1_000,
            )
            .unwrap();
        assert_eq!(revision.authority, AuthorityClass::OwnerAuthored);
        let heads = s.active_heads(Some(1_000)).unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].semantic_key, "communication.conclusion-first");
        assert_eq!(
            heads[0].statement,
            "Always give me the engineering conclusion first."
        );
    }

    /// Discrimination 2: observations remain candidates no matter how many
    /// times they repeat; nothing is ever active without an explicit
    /// owner action.
    #[test]
    fn repeated_observations_never_become_active() {
        let (_dir, s) = store();
        for i in 0..25 {
            s.record_observation(
                UserModelKind::Habit,
                "User keeps reformatting tables manually.",
                "formatting.tables",
                &format!("[\"turn-{i}\"]"),
                1_000 + i,
            )
            .unwrap();
        }
        assert!(
            s.active_heads(None).unwrap().is_empty(),
            "observations must never auto-promote to active heads"
        );
        // Even after accepting one sibling candidate, later repetitions
        // stay candidates; the active head stays exactly the ratified one.
        let accepted = s
            .record_observation(
                UserModelKind::Habit,
                "User keeps reformatting tables manually.",
                "formatting.tables",
                "[\"turn-99\"]",
                2_000,
            )
            .unwrap();
        s.review_candidate(
            &accepted.id,
            ReviewAction::Accept,
            "owner",
            None,
            None,
            2_100,
        )
        .unwrap();
        s.record_observation(
            UserModelKind::Habit,
            "User keeps reformatting tables manually.",
            "formatting.tables",
            "[\"turn-100\"]",
            3_000,
        )
        .unwrap();
        let heads = s.active_heads(None).unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].authority, AuthorityClass::OwnerRatified);
    }

    /// Discrimination 4: accept / reject / narrow / supersede produce
    /// distinct, append-only histories.
    #[test]
    fn review_actions_produce_distinct_histories() {
        let (_dir, s) = store();
        let candidate = s
            .record_observation(
                UserModelKind::Preference,
                "Prefers concise summaries.",
                "communication.summary-length",
                "[]",
                1_000,
            )
            .unwrap();

        let rejected = s
            .review_candidate(
                &candidate.id,
                ReviewAction::Reject,
                "owner",
                Some("not a habit"),
                None,
                2_000,
            )
            .unwrap();
        assert_eq!(rejected.action, ReviewAction::Reject);
        assert!(
            s.active_heads(Some(2_000)).unwrap().is_empty(),
            "reject must not activate anything"
        );
        // The candidate and its evidence survive the rejection.
        let conn = s.conn.lock();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM user_model_candidates WHERE id = ?1",
                rusqlite::params![candidate.id],
                |row| row.get(0),
            )
            .unwrap();
        drop(conn);
        assert_eq!(count, 1, "reject must not delete the candidate");

        let narrowed = s
            .review_candidate(
                &candidate.id,
                ReviewAction::Narrow,
                "owner",
                None,
                Some("session:trading"),
                3_000,
            )
            .unwrap();
        assert_eq!(narrowed.action, ReviewAction::Narrow);
        let heads = s.active_heads(Some(3_000)).unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].scope, "session:trading");

        let superseded_by = s.record_owner_statement(
            UserModelKind::Preference,
            "Prefers detailed summaries in deep-dive sessions.",
            "communication.summary-length",
            "global",
            4_000,
        );
        let superseded_by = superseded_by.unwrap();
        assert!(
            superseded_by.supersedes.is_some(),
            "a new statement must supersede the prior active revision"
        );
        // Append-only: every receipt and revision still exists.
        let conn = s.conn.lock();
        let receipts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM user_model_review_receipts",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let revisions: i64 = conn
            .query_row("SELECT COUNT(*) FROM user_model_revisions", [], |row| {
                row.get(0)
            })
            .unwrap();
        drop(conn);
        assert_eq!(receipts, 2);
        assert_eq!(revisions, 2);
    }

    #[test]
    fn candidate_history_uses_receipt_insertion_order_after_reopen() {
        let (dir, store) = store();
        let candidate = store
            .record_observation(UserModelKind::Habit, "habit", "habit.key", "[]", 100)
            .unwrap();
        store
            .conn
            .lock()
            .execute(
                "INSERT INTO user_model_review_receipts
                 (id, candidate_id, action, reviewer, note, at_unix)
                 VALUES ('zzzz-first', ?1, 'reject', 'owner', NULL, 200)",
                rusqlite::params![candidate.id],
            )
            .unwrap();
        let narrowed = store
            .review_candidate(
                &candidate.id,
                ReviewAction::Narrow,
                "owner",
                None,
                Some("session:A"),
                200,
            )
            .unwrap();
        assert!(narrowed.id.as_str() < "zzzz-first");
        store
            .conn
            .lock()
            .execute(
                "INSERT INTO user_model_review_receipts
                 (id, candidate_id, action, reviewer, note, at_unix)
                 VALUES ('aaaa-backdated', ?1, 'reject', 'owner', NULL, 199)",
                rusqlite::params![candidate.id],
            )
            .unwrap();
        drop(store);

        let reopened = UserModelStore::open(dir.path()).unwrap();
        let before = review_scope_snapshot(&reopened);
        let (found, receipts) = reopened.candidate_history(&candidate.id).unwrap().unwrap();
        assert_eq!(found, candidate);
        assert_eq!(
            receipts
                .iter()
                .map(|receipt| receipt.id.as_str())
                .collect::<Vec<_>>(),
            vec!["zzzz-first", narrowed.id.as_str(), "aaaa-backdated"]
        );
        assert_eq!(receipts.last().unwrap().action, ReviewAction::Reject);
        assert_eq!(review_scope_snapshot(&reopened), before);
    }

    /// Discrimination 5: as-of reads return the correct revision across a
    /// supersession boundary.
    #[test]
    fn as_of_reads_respect_supersession() {
        let (_dir, s) = store();
        let first = s
            .record_owner_statement(
                UserModelKind::Goal,
                "Ship the A-share harness first.",
                "goal.priority",
                "global",
                1_000,
            )
            .unwrap();
        s.record_owner_statement(
            UserModelKind::Goal,
            "Ship the companion memory first.",
            "goal.priority",
            "global",
            3_000,
        )
        .unwrap();

        let at_start = s.active_heads(Some(1_500)).unwrap();
        assert_eq!(at_start.len(), 1);
        assert_eq!(
            at_start[0].id, first.id,
            "before supersession the first revision is the head"
        );

        let now = s.active_heads(Some(3_500)).unwrap();
        assert_eq!(now.len(), 1);
        assert_eq!(now[0].statement, "Ship the companion memory first.");

        let before_anything = s.active_heads(Some(500)).unwrap();
        assert!(before_anything.is_empty());
    }

    /// Expiry: a revision with valid_until stops being projected after it
    /// expires (task-scoped requests ride the same mechanism).
    #[test]
    fn expired_revisions_leave_the_active_heads() {
        let (_dir, s) = store();
        s.record_owner_statement(
            UserModelKind::Preference,
            "For this task use Codex.",
            "task.model-choice",
            "session:one-off",
            1_000,
        )
        .unwrap();
        let heads = s.active_heads(Some(1_200)).unwrap();
        assert_eq!(heads.len(), 1);

        // Supersede it with a bounded (expiring) revision, then read past
        // its end: nothing may be active for that key anymore.
        let bounded = s
            .record_owner_statement(
                UserModelKind::Preference,
                "For this task use Codex.",
                "task.model-choice",
                "session:one-off",
                2_000,
            )
            .unwrap();
        let conn = s.conn.lock();
        conn.execute(
            "UPDATE user_model_revisions SET valid_until_unix = ?1 WHERE id = ?2",
            rusqlite::params![3_000, bounded.id],
        )
        .unwrap();
        drop(conn);
        assert!(s.active_heads(Some(3_500)).unwrap().is_empty());
        assert_eq!(s.active_heads(Some(2_500)).unwrap().len(), 1);
    }

    /// Same-timestamp revisions for one key must resolve to exactly one
    /// head, stably across reads (tie broken by id, deterministic).
    #[test]
    fn same_timestamp_tie_resolves_to_one_stable_head() {
        let (_dir, s) = store();
        s.record_owner_statement(
            UserModelKind::Preference,
            "First statement in the same second.",
            "communication.tie",
            "global",
            5_000,
        )
        .unwrap();
        s.record_owner_statement(
            UserModelKind::Preference,
            "Second statement in the same second.",
            "communication.tie",
            "global",
            5_000,
        )
        .unwrap();
        let first_read = s.active_heads(Some(5_000)).unwrap();
        assert_eq!(first_read.len(), 1, "a tie must yield exactly one head");
        let second_read = s.active_heads(Some(5_000)).unwrap();
        assert_eq!(
            first_read[0].id, second_read[0].id,
            "the tie winner must be stable across reads"
        );
    }

    /// Projection: empty heads render nothing; the section is bounded with
    /// newest-first priority and an elision count; ids ride alongside for
    /// internal correction without entering the prompt text.
    #[test]
    fn projection_is_bounded_and_reports_ids() {
        let (_dir, s) = store();
        for i in 0..40 {
            s.record_owner_statement(
                UserModelKind::Preference,
                &format!("Preference number {i} with some words to consume budget."),
                &format!("pref.{i}"),
                "global",
                1_000 + i,
            )
            .unwrap();
        }
        let heads = s.active_heads(None).unwrap();
        let projection = project_active_heads(&heads, 600);
        assert!(
            projection.prompt_section.len() <= 700,
            "section must stay near the budget"
        );
        assert!(
            projection.prompt_section.contains("elided"),
            "a 40-head set over a 600-char budget must elide"
        );
        assert!(
            !projection.prompt_section.contains(&heads[0].id),
            "revision ids must not enter the prompt text"
        );
        assert!(!projection.revision_ids.is_empty());

        let empty = project_active_heads(&[], 600);
        assert!(empty.prompt_section.is_empty());
        assert!(empty.revision_ids.is_empty());
    }

    #[test]
    fn projection_prefers_newest_heads() {
        let (_dir, s) = store();
        s.record_owner_statement(
            UserModelKind::Value,
            "Old value statement.",
            "value.only",
            "global",
            1_000,
        )
        .unwrap();
        s.record_owner_statement(
            UserModelKind::Goal,
            "Newest goal statement.",
            "goal.only",
            "global",
            2_000,
        )
        .unwrap();
        let heads = s.active_heads(None).unwrap();
        let projection = project_active_heads(&heads, 4_000);
        let goal_pos = projection
            .prompt_section
            .find("Newest goal statement.")
            .unwrap();
        let value_pos = projection
            .prompt_section
            .find("Old value statement.")
            .unwrap();
        assert!(
            goal_pos < value_pos,
            "newest revisions must be rendered first"
        );
        assert_eq!(projection.revision_ids.len(), 2);
    }

    /// Durable across reopen: local-first means no daemon lifetime magic.
    #[test]
    fn store_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = UserModelStore::open(dir.path()).unwrap();
            s.record_owner_statement(
                UserModelKind::Value,
                "Privacy over convenience.",
                "value.privacy",
                "global",
                1_000,
            )
            .unwrap();
        }
        let reopened = UserModelStore::open(dir.path()).unwrap();
        assert_eq!(reopened.active_heads(None).unwrap().len(), 1);
    }

    #[test]
    fn current_time_heads_include_live_windows_and_exclude_future_starts() {
        let (_dir, s) = store();
        assert!(s.active_heads(None).unwrap().is_empty());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let past = now.checked_sub(3_600).unwrap();
        let future = now.checked_add(3_600).unwrap();
        for (key, starts) in [
            ("active", past),
            ("future-start", future),
            ("future-expiry", past),
        ] {
            let revision = s
                .record_owner_statement(UserModelKind::Preference, key, key, "global", starts)
                .unwrap();
            if key == "future-expiry" {
                s.conn
                    .lock()
                    .execute(
                        "UPDATE user_model_revisions SET valid_until_unix = ?1 WHERE id = ?2",
                        rusqlite::params![future, revision.id],
                    )
                    .unwrap();
            }
        }
        let heads = s.active_heads(None).unwrap();
        let keys: Vec<_> = heads
            .iter()
            .map(|head| head.semantic_key.as_str())
            .collect();
        eprintln!(
            "USER_MODEL_CURRENT_TIME_OBSERVED active={} future_start={} future_expiry={}",
            keys.contains(&"active"),
            keys.contains(&"future-start"),
            keys.contains(&"future-expiry")
        );
        assert_eq!(keys, vec!["active", "future-expiry"]);
        assert_eq!(s.active_heads(Some(now)).unwrap(), heads);
    }
    pub(super) fn review_scope_snapshot(
        store: &UserModelStore,
    ) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
        let conn = store.conn.lock();
        [
            "user_model_candidates",
            "user_model_review_receipts",
            "user_model_revisions",
        ]
        .iter()
        .map(|table| {
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY id"))
                .unwrap();
            let columns = stmt.column_count();
            stmt.query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        })
        .collect()
    }

    #[test]
    fn guarded_review_compares_exact_head_before_any_write() {
        use zeroclaw_api::companion::UserModelExpectedHead;
        let (_dir, store) = store();
        for key in ["changed", "appeared"] {
            let initial = if key == "changed" {
                Some(
                    store
                        .record_owner_statement(UserModelKind::Preference, "A", key, "global", 1)
                        .unwrap()
                        .id,
                )
            } else {
                None
            };
            let candidate = store
                .record_observation(UserModelKind::Preference, "Candidate", key, "[]", 2)
                .unwrap();
            let latest = store
                .record_owner_statement(UserModelKind::Preference, "B", key, "global", 3)
                .unwrap();
            let before = review_scope_snapshot(&store);
            let result = store.review_candidate_with_expected_head(
                &candidate.id,
                ReviewAction::Accept,
                "owner",
                None,
                None,
                None,
                Some(&UserModelExpectedHead { id: initial }),
                4,
            );
            assert!(is_user_model_head_conflict(&result.unwrap_err()));
            assert_eq!(review_scope_snapshot(&store), before);
            store
                .review_candidate_with_expected_head(
                    &candidate.id,
                    ReviewAction::Accept,
                    "owner",
                    None,
                    None,
                    None,
                    Some(&UserModelExpectedHead {
                        id: Some(latest.id.clone()),
                    }),
                    4,
                )
                .unwrap();
            let head = store
                .active_heads(Some(4))
                .unwrap()
                .into_iter()
                .find(|head| head.semantic_key == key)
                .unwrap();
            assert_eq!(head.supersedes.as_deref(), Some(latest.id.as_str()));
        }
        let fresh = store
            .record_observation(UserModelKind::Preference, "New", "empty", "[]", 5)
            .unwrap();
        store
            .review_candidate_with_expected_head(
                &fresh.id,
                ReviewAction::Narrow,
                "owner",
                None,
                Some("session:one"),
                None,
                Some(&UserModelExpectedHead { id: None }),
                6,
            )
            .unwrap();
        let head = store
            .active_heads(Some(6))
            .unwrap()
            .into_iter()
            .find(|head| head.semantic_key == "empty")
            .unwrap();
        assert!(head.supersedes.is_none());
    }

    #[test]
    fn committed_review_repeats_preserve_all_rows_except_reject_then_narrow() {
        let (_dir, store) = store();
        for first in [
            ReviewAction::Accept,
            ReviewAction::Narrow,
            ReviewAction::Supersede,
        ] {
            let candidate = store
                .record_observation(
                    UserModelKind::Habit,
                    "habit",
                    &format!("key.{first:?}"),
                    "[]",
                    100,
                )
                .unwrap();
            store
                .review_candidate(&candidate.id, first, "owner", None, Some("session:A"), 200)
                .unwrap();
            let before = review_scope_snapshot(&store);
            for repeat in [
                ReviewAction::Accept,
                ReviewAction::Reject,
                ReviewAction::Narrow,
                ReviewAction::Supersede,
            ] {
                let error = store
                    .review_candidate(&candidate.id, repeat, "owner", None, Some("session:A"), 201)
                    .unwrap_err();
                assert!(
                    is_candidate_already_reviewed(&error),
                    "{first:?} then {repeat:?}: {error}"
                );
                assert_eq!(review_scope_snapshot(&store), before);
            }
        }

        let candidate = store
            .record_observation(UserModelKind::Habit, "rejected", "rejected.key", "[]", 100)
            .unwrap();
        store
            .review_candidate(
                &candidate.id,
                ReviewAction::Reject,
                "owner",
                None,
                None,
                200,
            )
            .unwrap();
        let rejected = review_scope_snapshot(&store);
        for repeat in [
            ReviewAction::Accept,
            ReviewAction::Reject,
            ReviewAction::Supersede,
        ] {
            let error = store
                .review_candidate(&candidate.id, repeat, "owner", None, None, 201)
                .unwrap_err();
            assert!(
                is_candidate_already_reviewed(&error),
                "Reject then {repeat:?}: {error}"
            );
            assert_eq!(review_scope_snapshot(&store), rejected);
        }
        let invalid = store
            .review_candidate(
                &candidate.id,
                ReviewAction::Narrow,
                "owner",
                None,
                Some("invalid"),
                201,
            )
            .unwrap_err();
        assert!(matches!(invalid, rusqlite::Error::InvalidParameterName(_)));
        assert_eq!(review_scope_snapshot(&store), rejected);
        store
            .review_candidate(
                &candidate.id,
                ReviewAction::Narrow,
                "owner",
                None,
                Some("session:A"),
                202,
            )
            .unwrap();
        let narrowed = review_scope_snapshot(&store);
        assert_eq!(narrowed[1].len(), rejected[1].len() + 1);
        assert_eq!(narrowed[2].len(), rejected[2].len() + 1);
        for repeat in [
            ReviewAction::Accept,
            ReviewAction::Reject,
            ReviewAction::Narrow,
            ReviewAction::Supersede,
        ] {
            let error = store
                .review_candidate(&candidate.id, repeat, "owner", None, Some("session:A"), 203)
                .unwrap_err();
            assert!(
                is_candidate_already_reviewed(&error),
                "Narrow then {repeat:?}: {error}"
            );
            assert_eq!(review_scope_snapshot(&store), narrowed);
        }
    }

    #[test]
    fn independent_connections_serialize_review_decisions() {
        let dir = tempfile::tempdir().unwrap();
        let store = UserModelStore::open(dir.path()).unwrap();
        let candidate = store
            .record_observation(UserModelKind::Habit, "habit", "concurrent.key", "[]", 100)
            .unwrap();
        let run_pair = |action: ReviewAction| {
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
            let mut handles = Vec::new();
            for _ in 0..2 {
                let separate = UserModelStore::open(dir.path()).unwrap();
                let id = candidate.id.clone();
                let barrier = barrier.clone();
                handles.push(std::thread::spawn(move || {
                    barrier.wait();
                    separate.review_candidate(&id, action, "owner", None, Some("session:A"), 200)
                }));
            }
            barrier.wait();
            let results: Vec<_> = handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect();
            assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
            assert_eq!(results.iter().filter(|result| matches!(result, Err(error) if is_candidate_already_reviewed(error))).count(), 1);
        };
        run_pair(ReviewAction::Accept);
        let after_accept = review_scope_snapshot(&store);
        assert_eq!(after_accept[1].len(), 1);
        assert_eq!(after_accept[2].len(), 1);

        let rejected = store
            .record_observation(
                UserModelKind::Habit,
                "other",
                "rejected.concurrent",
                "[]",
                100,
            )
            .unwrap();
        store
            .review_candidate(&rejected.id, ReviewAction::Reject, "owner", None, None, 200)
            .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let separate = UserModelStore::open(dir.path()).unwrap();
            let id = rejected.id.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                separate.review_candidate(
                    &id,
                    ReviewAction::Narrow,
                    "owner",
                    None,
                    Some("session:A"),
                    201,
                )
            }));
        }
        barrier.wait();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(
                    |result| matches!(result, Err(error) if is_candidate_already_reviewed(error))
                )
                .count(),
            1
        );
        let after_narrow = review_scope_snapshot(&store);
        assert_eq!(after_narrow[1].len(), after_accept[1].len() + 2);
        assert_eq!(after_narrow[2].len(), after_accept[2].len() + 1);
    }

    #[test]
    fn unknown_stored_review_action_fails_without_writing() {
        let (_dir, store) = store();
        let candidate = store
            .record_observation(UserModelKind::Habit, "habit", "unknown.action", "[]", 100)
            .unwrap();
        store
            .conn
            .lock()
            .execute(
                "INSERT INTO user_model_review_receipts
             (id, candidate_id, action, reviewer, note, at_unix)
             VALUES ('unknown-action', ?1, 'unsupported', 'owner', NULL, 200)",
                rusqlite::params![candidate.id],
            )
            .unwrap();
        let before = review_scope_snapshot(&store);
        let error = store
            .review_candidate(
                &candidate.id,
                ReviewAction::Narrow,
                "owner",
                None,
                Some("session:A"),
                201,
            )
            .unwrap_err();
        assert!(matches!(error, rusqlite::Error::InvalidQuery));
        assert_eq!(review_scope_snapshot(&store), before);
    }

    #[test]
    fn invalid_review_scope_preserves_all_rows_before_receipt() {
        let (_dir, store) = store();
        let candidate = store
            .record_observation(
                UserModelKind::Habit,
                "private observation",
                "private.key",
                "[]",
                100,
            )
            .unwrap();
        let before = review_scope_snapshot(&store);
        for scope in ["", "task:unsupported", "unknown:scope"] {
            let error = store
                .review_candidate(
                    &candidate.id,
                    ReviewAction::Narrow,
                    "owner",
                    None,
                    Some(scope),
                    200,
                )
                .unwrap_err();
            assert!(
                matches!(error, rusqlite::Error::InvalidParameterName(ref message) if message == &format!("invalid narrowed scope '{scope}'"))
            );
            assert_eq!(review_scope_snapshot(&store), before);
        }
        assert!(matches!(
            store.review_candidate(
                "missing",
                ReviewAction::Narrow,
                "owner",
                None,
                Some("invalid"),
                200
            ),
            Err(rusqlite::Error::QueryReturnedNoRows)
        ));
        assert_eq!(review_scope_snapshot(&store), before);
        let receipt = store
            .review_candidate(
                &candidate.id,
                ReviewAction::Narrow,
                "owner",
                None,
                Some("session:A"),
                200,
            )
            .unwrap();
        assert_eq!(receipt.action, ReviewAction::Narrow);
        let after = review_scope_snapshot(&store);
        assert_eq!(after[0], before[0]);
        assert_eq!(after[1].len(), 1);
        assert_eq!(after[2].len(), 1);
        let heads = store.active_heads(Some(200)).unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].scope, "session:A");
        assert_eq!(heads[0].authority, AuthorityClass::OwnerRatified);
    }
    #[test]
    fn review_insert_failure_rolls_back_receipt_and_preserves_reject() {
        let (_dir, store) = store();
        let candidate = store
            .record_observation(UserModelKind::Habit, "private", "atomic.key", "[]", 100)
            .unwrap();
        store.conn.lock().execute_batch("CREATE TEMP TRIGGER review_insert_fault BEFORE INSERT ON main.user_model_revisions BEGIN SELECT RAISE(ABORT, 'private revision insert fault'); END;").unwrap();
        let before = review_scope_snapshot(&store);
        let error = store
            .review_candidate(
                &candidate.id,
                ReviewAction::Accept,
                "owner",
                None,
                None,
                200,
            )
            .unwrap_err();
        assert!(error.to_string().contains("private revision insert fault"));
        assert_eq!(review_scope_snapshot(&store), before);
        assert!(store.conn.lock().is_autocommit());
        let rejected = store
            .review_candidate(
                &candidate.id,
                ReviewAction::Reject,
                "owner",
                None,
                Some("ignored"),
                201,
            )
            .unwrap();
        assert_eq!(rejected.action, ReviewAction::Reject);
        let after_reject = review_scope_snapshot(&store);
        assert_eq!(after_reject[0], before[0]);
        assert_eq!(after_reject[1].len(), 1);
        assert_eq!(after_reject[2], before[2]);
        store
            .conn
            .lock()
            .execute_batch("DROP TRIGGER temp.review_insert_fault;")
            .unwrap();
        store
            .review_candidate(
                &candidate.id,
                ReviewAction::Narrow,
                "owner",
                None,
                Some("session:A"),
                202,
            )
            .unwrap();
        let after = review_scope_snapshot(&store);
        assert_eq!(after[0], before[0]);
        assert_eq!(after[1].len(), 2);
        assert_eq!(after[2].len(), 1);
        assert_eq!(
            store.active_heads(Some(202)).unwrap()[0].authority,
            AuthorityClass::OwnerRatified
        );
    }

    #[test]
    fn review_commit_failure_rolls_back_canonical_and_auxiliary_rows() {
        let (_dir, store) = store();
        let candidate = store
            .record_observation(UserModelKind::Habit, "private", "atomic.key", "[]", 100)
            .unwrap();
        store
            .record_owner_statement(
                UserModelKind::Habit,
                "baseline",
                "baseline.key",
                "global",
                100,
            )
            .unwrap();
        {
            let conn = store.conn.lock();
            conn.execute_batch("PRAGMA foreign_keys = ON;
                CREATE TEMP TABLE review_fault_parent (id INTEGER PRIMARY KEY);
                CREATE TEMP TABLE review_fault_child (parent_id INTEGER REFERENCES review_fault_parent(id) DEFERRABLE INITIALLY DEFERRED);
                CREATE TEMP TRIGGER review_commit_fault AFTER INSERT ON main.user_model_revisions BEGIN INSERT INTO review_fault_child VALUES (1); END;").unwrap();
            assert_eq!(
                conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
        let before = review_scope_snapshot(&store);
        // Calibrate the exact trigger: revision INSERT succeeds, explicit
        // COMMIT fails. A statement-level abort is not this discriminator.
        {
            let mut conn = store.conn.lock();
            let tx = conn.transaction().unwrap();
            assert_eq!(tx.execute("INSERT INTO user_model_revisions
                (id, semantic_key, kind, statement, scope, authority, supersedes, valid_from_unix, valid_until_unix, source_candidate, created_at_unix)
                SELECT 'private-calibration', semantic_key, kind, statement, scope, authority, supersedes, valid_from_unix, valid_until_unix, source_candidate, created_at_unix FROM user_model_revisions LIMIT 1", []).unwrap(), 1);
            assert_eq!(
                tx.query_row("SELECT COUNT(*) FROM review_fault_child", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            let error = tx.commit().unwrap_err();
            assert!(
                matches!(error, rusqlite::Error::SqliteFailure(ref code, _) if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY)
            );
            assert!(conn.is_autocommit());
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM review_fault_child", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        assert_eq!(review_scope_snapshot(&store), before);
        let error = store
            .review_candidate(
                &candidate.id,
                ReviewAction::Accept,
                "owner",
                None,
                None,
                200,
            )
            .unwrap_err();
        assert!(
            matches!(error, rusqlite::Error::SqliteFailure(ref code, _) if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY)
        );
        assert_eq!(review_scope_snapshot(&store), before);
        {
            let conn = store.conn.lock();
            assert!(conn.is_autocommit());
            for table in ["review_fault_child", "review_fault_parent"] {
                assert_eq!(
                    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))
                        .unwrap(),
                    0
                );
            }
            conn.execute_batch("DROP TRIGGER temp.review_commit_fault; DROP TABLE temp.review_fault_child; DROP TABLE temp.review_fault_parent;").unwrap();
        }
        store
            .review_candidate(
                &candidate.id,
                ReviewAction::Accept,
                "owner",
                None,
                None,
                201,
            )
            .unwrap();
        let after = review_scope_snapshot(&store);
        assert_eq!(after[1].len(), 1);
        assert_eq!(after[2].len(), 2);
    }
    #[test]
    fn candidate_pages_keep_ties_and_newer_inserts_out_of_older_pages() {
        let (_dir, store) = store();
        assert_eq!(
            store.list_candidates_page(false, 2, None).unwrap(),
            (vec![], false)
        );
        for (index, at) in [100, 100, 100, 99, 0].into_iter().enumerate() {
            store
                .record_observation(
                    UserModelKind::Habit,
                    &format!("Candidate {index}"),
                    &format!("key.{index}"),
                    &format!("evidence {index}"),
                    at,
                )
                .unwrap();
        }
        let expected = store.list_candidates().unwrap();
        let (first, more) = store.list_candidates_page(false, 2, None).unwrap();
        assert_eq!(first, expected[..2]);
        assert!(more);
        store
            .record_observation(UserModelKind::Habit, "Newer", "newer", "new evidence", 101)
            .unwrap();
        let before = review_scope_snapshot(&store);
        let last = first.last().unwrap();
        let (second, more) = store
            .list_candidates_page(false, 2, Some((last.created_at_unix, &last.id)))
            .unwrap();
        assert_eq!(second, expected[2..4]);
        assert!(more);
        let last = second.last().unwrap();
        let (end, more) = store
            .list_candidates_page(false, 2, Some((last.created_at_unix, &last.id)))
            .unwrap();
        assert_eq!(end, expected[4..]);
        assert!(!more);
        let last = end.last().unwrap();
        assert_eq!(
            store
                .list_candidates_page(false, 2, Some((last.created_at_unix, &last.id)))
                .unwrap(),
            (vec![], false)
        );
        assert_eq!(review_scope_snapshot(&store), before);
        for limit in [0, 201, usize::MAX] {
            assert!(store.list_candidates_page(false, limit, None).is_err());
        }
        assert!(
            store
                .list_candidates_page(false, 2, Some((u64::MAX, "x")))
                .is_err()
        );
    }

    #[test]
    fn candidate_pages_filter_committed_reviews_and_only_read_limited_evidence() {
        let (_dir, store) = store();
        for at in 1..=4 {
            store
                .record_observation(
                    UserModelKind::Habit,
                    &format!("Candidate {at}"),
                    &format!("key.{at}"),
                    "private evidence",
                    at,
                )
                .unwrap();
        }
        let expected = store.list_candidates().unwrap();
        store
            .review_candidate(
                &expected[0].id,
                ReviewAction::Reject,
                "owner",
                None,
                None,
                10,
            )
            .unwrap();
        let before = review_scope_snapshot(&store);
        let (pending, more) = store.list_candidates_page(true, 2, None).unwrap();
        assert_eq!(pending, expected[1..3]);
        assert!(more);
        let last = pending.last().unwrap();
        assert_eq!(
            store
                .list_candidates_page(true, 2, Some((last.created_at_unix, &last.id)))
                .unwrap(),
            (expected[3..].to_vec(), false)
        );
        assert_eq!(review_scope_snapshot(&store), before);
        // The oldest row is outside the first page and its one-row lookahead.
        // A full evidence read would fail on its deliberately invalid SQL type.
        store
            .conn
            .lock()
            .execute(
                "UPDATE user_model_candidates SET evidence = X'00' WHERE id = ?1",
                [&expected[3].id],
            )
            .unwrap();
        assert!(store.list_candidates().is_err());
        assert_eq!(
            store.list_candidates_page(false, 2, None).unwrap(),
            (expected[..2].to_vec(), true)
        );
        let last = &expected[1];
        assert!(
            store
                .list_candidates_page(false, 2, Some((last.created_at_unix, &last.id)))
                .is_err()
        );
    }

    #[test]
    fn pending_candidates_derive_only_from_committed_receipts() {
        let (_dir, store) = store();
        assert!(store.list_pending_candidates().unwrap().is_empty());
        let c = store
            .record_observation(UserModelKind::Habit, "C", "c", "[]", 100)
            .unwrap();
        let d = store
            .record_observation(UserModelKind::Habit, "D", "d", "[]", 101)
            .unwrap();
        assert_eq!(
            store.list_pending_candidates().unwrap(),
            vec![d.clone(), c.clone()]
        );
        assert!(
            store
                .review_candidate(
                    &c.id,
                    ReviewAction::Narrow,
                    "owner",
                    None,
                    Some("invalid"),
                    200
                )
                .is_err()
        );
        assert_eq!(
            store.list_pending_candidates().unwrap(),
            vec![d.clone(), c.clone()]
        );
        store.conn.lock().execute_batch("CREATE TEMP TRIGGER pending_revision_fault BEFORE INSERT ON main.user_model_revisions BEGIN SELECT RAISE(ABORT, 'private pending fault'); END;").unwrap();
        assert!(
            store
                .review_candidate(&d.id, ReviewAction::Accept, "owner", None, None, 200)
                .is_err()
        );
        assert_eq!(
            store.list_pending_candidates().unwrap(),
            vec![d.clone(), c.clone()]
        );
        store
            .conn
            .lock()
            .execute_batch("DROP TRIGGER temp.pending_revision_fault;")
            .unwrap();
        store
            .review_candidate(&c.id, ReviewAction::Reject, "owner", None, None, 200)
            .unwrap();
        assert_eq!(store.list_pending_candidates().unwrap(), vec![d.clone()]);
        store
            .review_candidate(&d.id, ReviewAction::Accept, "owner", None, None, 201)
            .unwrap();
        assert!(store.list_pending_candidates().unwrap().is_empty());
        assert_eq!(store.list_candidates().unwrap(), vec![d, c]);
        let e = store
            .record_observation(UserModelKind::Habit, "E", "e", "[]", 202)
            .unwrap();
        let before = review_scope_snapshot(&store);
        assert_eq!(store.list_pending_candidates().unwrap(), vec![e]);
        assert_eq!(store.list_candidates().unwrap().len(), 3);
        assert_eq!(review_scope_snapshot(&store), before);
        assert_eq!(
            store.active_heads(Some(202)).unwrap()[0].authority,
            AuthorityClass::OwnerRatified
        );
    }
    #[test]
    fn reflection_queue_bound_is_enforced_across_connections() {
        let dir = tempfile::tempdir().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let store = UserModelStore::open(dir.path()).unwrap();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .record_reflection_observation(
                            UserModelKind::Preference,
                            &format!("Preference {i}"),
                            &format!("pref.{i}"),
                            "[]",
                            100,
                        )
                        .unwrap()
                        .is_some()
                })
            })
            .collect();
        let created = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|created| *created)
            .count();
        assert_eq!(created, USER_MODEL_MAX_OPEN_REFLECTION_CANDIDATES);
        let store = UserModelStore::open(dir.path()).unwrap();
        assert_eq!(store.list_pending_candidates().unwrap().len(), 3);
        assert!(store.active_heads(Some(101)).unwrap().is_empty());
    }

    #[test]
    fn reword_is_atomic_preserves_original_and_invalid_text_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = UserModelStore::open(dir.path()).unwrap();
        let candidate = store
            .record_observation(
                UserModelKind::Preference,
                "original",
                "pref.original",
                "[{\"session_id\":\"s1\"}]",
                100,
            )
            .unwrap();
        for text in [
            "".to_string(),
            "\ninvalid".to_string(),
            "x".repeat(USER_MODEL_STATEMENT_MAX_BYTES + 1),
        ] {
            assert!(
                store
                    .review_candidate_with_text(
                        &candidate.id,
                        ReviewAction::Accept,
                        "operator",
                        None,
                        None,
                        Some(&text),
                        101
                    )
                    .is_err()
            );
        }
        let history = store.candidate_history(&candidate.id).unwrap().unwrap();
        assert_eq!(history.0, candidate);
        assert!(history.1.is_empty());
        let conn = Connection::open(dir.path().join("user_model.db")).unwrap();
        conn.execute_batch("CREATE TRIGGER reword_fault BEFORE INSERT ON user_model_revisions BEGIN SELECT RAISE(ABORT,'reword fault'); END;").unwrap();
        assert!(
            store
                .review_candidate_with_text(
                    &candidate.id,
                    ReviewAction::Accept,
                    "operator",
                    None,
                    None,
                    Some("owner wording"),
                    101
                )
                .is_err()
        );
        assert!(
            store
                .candidate_history(&candidate.id)
                .unwrap()
                .unwrap()
                .1
                .is_empty()
        );
        assert!(store.active_heads(Some(102)).unwrap().is_empty());
        conn.execute_batch("DROP TRIGGER reword_fault;").unwrap();
        store
            .review_candidate_with_text(
                &candidate.id,
                ReviewAction::Accept,
                "operator",
                None,
                None,
                Some("owner wording"),
                101,
            )
            .unwrap();
        let heads = UserModelStore::open(dir.path())
            .unwrap()
            .active_heads(Some(102))
            .unwrap();
        assert_eq!(heads[0].statement, "owner wording");
        assert_eq!(
            store.candidate_history(&candidate.id).unwrap().unwrap().0,
            candidate
        );
    }
}

#[cfg(test)]
mod owner_correction_tests {
    use super::tests::review_scope_snapshot;
    use super::*;

    fn evidence(agent: &str, session: &str, key: &str) -> String {
        serde_json::json!({"origin":"owner_correction", "agent":agent, "submitted_semantic_key":key,
            "messages":[{"session_id":session, "at_unix":1, "owner_text":"Owner correction.", "source":{"kind":"operator"}}]}).to_string()
    }

    #[test]
    fn owner_correction_same_second_approval_uses_insertion_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = UserModelStore::open(dir.path()).unwrap();
        for (statement, forced_id) in [("Earlier", "zzzz"), ("Middle", "aaaa"), ("Later", "bbbb")] {
            let candidate = store
                .record_owner_correction(
                    "nova",
                    UserModelKind::Preference,
                    statement,
                    "style",
                    &evidence("nova", "s", "style"),
                    "s",
                    1,
                )
                .unwrap()
                .unwrap();
            store
                .review_candidate(
                    &candidate.id,
                    ReviewAction::Accept,
                    "operator",
                    None,
                    None,
                    2,
                )
                .unwrap();
            store
                .conn
                .lock()
                .execute(
                    "UPDATE user_model_revisions SET id = ?1 WHERE source_candidate = ?2",
                    rusqlite::params![forced_id, candidate.id],
                )
                .unwrap();
        }
        let heads = store.active_heads_for_agent("nova", Some(2)).unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].statement, "Later");
        assert_eq!(heads[0].supersedes.as_deref(), Some("aaaa"));
        let next = store
            .record_owner_correction(
                "nova",
                UserModelKind::Preference,
                "Next",
                "style",
                &evidence("nova", "s", "style"),
                "s",
                2,
            )
            .unwrap()
            .unwrap();
        let stale = zeroclaw_api::companion::UserModelExpectedHead {
            id: Some("zzzz".into()),
        };
        let before = review_scope_snapshot(&store);
        assert!(is_user_model_head_conflict(
            &store
                .review_candidate_with_expected_head(
                    &next.id,
                    ReviewAction::Accept,
                    "owner",
                    None,
                    None,
                    None,
                    Some(&stale),
                    2
                )
                .unwrap_err()
        ));
        assert_eq!(review_scope_snapshot(&store), before);
        // Expiring the latest same-second head must not revive its predecessor.
        store
            .conn
            .lock()
            .execute(
                "UPDATE user_model_revisions SET valid_until_unix = 3 WHERE id = 'bbbb'",
                [],
            )
            .unwrap();
        assert!(
            store
                .active_heads_for_agent("nova", Some(3))
                .unwrap()
                .is_empty()
        );
        let empty = zeroclaw_api::companion::UserModelExpectedHead { id: None };
        store
            .review_candidate_with_expected_head(
                &next.id,
                ReviewAction::Accept,
                "owner",
                None,
                None,
                None,
                Some(&empty),
                4,
            )
            .unwrap();
        let heads = store.active_heads_for_agent("nova", Some(4)).unwrap();
        assert_eq!(heads[0].statement, "Next");
        assert!(
            heads[0].supersedes.is_none(),
            "expired head must not revive during guarded approval"
        );
    }

    #[test]
    fn owner_correction_exact_shape_preserves_historical_statement_and_global_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let store = UserModelStore::open(dir.path()).unwrap();
        let key = format!("oc.{}", "a".repeat(60));
        store
            .record_owner_statement(
                UserModelKind::Preference,
                "Historical owner",
                &key,
                "global",
                1,
            )
            .unwrap();
        assert_eq!(
            store.active_heads_for_agent("any", Some(1)).unwrap()[0].statement,
            "Historical owner"
        );
        let candidate = store
            .record_observation(
                UserModelKind::Preference,
                "Historical candidate",
                &key,
                "[]",
                2,
            )
            .unwrap();
        assert!(candidate.visible_to_agent("any"));
        store
            .review_candidate(
                &candidate.id,
                ReviewAction::Narrow,
                "operator",
                None,
                Some("session:s"),
                3,
            )
            .unwrap();
        let heads = store.active_heads_for_agent("any", Some(3)).unwrap();
        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].statement, "Historical candidate");
        assert_eq!(heads[0].scope, "session:s");
    }

    #[test]
    fn owner_correction_projection_resolves_canonical_agent_and_rejects_corrupt_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let store = UserModelStore::open(dir.path()).unwrap();
        store
            .record_owner_statement(
                UserModelKind::Preference,
                "Global baseline.",
                "style",
                "global",
                1,
            )
            .unwrap();
        let mut candidates = Vec::new();
        for agent in ["nova", "other"] {
            let candidate = store
                .record_owner_correction(
                    agent,
                    UserModelKind::Preference,
                    &format!("{agent} correction."),
                    "style",
                    &evidence(agent, "same-session", "style"),
                    "same-session",
                    2,
                )
                .unwrap()
                .unwrap();
            assert!(candidate.visible_to_agent(agent));
            assert!(!candidate.visible_to_agent("unrelated"));
            store
                .review_candidate(
                    &candidate.id,
                    ReviewAction::Accept,
                    "operator",
                    None,
                    None,
                    3,
                )
                .unwrap();
            candidates.push(candidate);
        }
        assert_ne!(candidates[0].semantic_key, candidates[1].semantic_key);
        for agent in ["nova", "other"] {
            let heads = store.active_heads_for_agent(agent, Some(3)).unwrap();
            assert_eq!(heads.len(), 2);
            let projection = project_applicable_heads(
                heads,
                &ApplicabilityContext::new(agent, "test", "same-session"),
                1200,
            );
            assert!(
                projection
                    .prompt_section
                    .contains(&format!("{agent} correction."))
            );
            assert!(projection.prompt_section.contains("Global baseline."));
        }
        for broken in [
            "not JSON",
            "{}",
            &evidence("other", "same-session", "style"),
            &evidence("nova", "another-session", "style"),
        ] {
            store
                .conn
                .lock()
                .execute(
                    "UPDATE user_model_candidates SET evidence = ?1 WHERE id = ?2",
                    rusqlite::params![broken, candidates[0].id],
                )
                .unwrap();
            let heads = store.active_heads_for_agent("nova", Some(3)).unwrap();
            assert_eq!(
                heads.len(),
                1,
                "malformed correction must not become shared: {broken}"
            );
            assert_eq!(heads[0].statement, "Global baseline.");
        }
        assert!(
            store
                .record_owner_correction(
                    "nova",
                    UserModelKind::Preference,
                    "Invalid provenance.",
                    "style",
                    "{}",
                    "same-session",
                    4
                )
                .is_err()
        );
    }

    #[test]
    fn owner_correction_keys_isolate_sessions_and_global_heads() {
        let dir = tempfile::tempdir().unwrap();
        let store = UserModelStore::open(dir.path()).unwrap();
        store
            .record_owner_statement(
                UserModelKind::Preference,
                "Global baseline.",
                "style",
                "global",
                1,
            )
            .unwrap();
        let a = store
            .record_owner_correction(
                "nova",
                UserModelKind::Preference,
                "A correction.",
                "style",
                &evidence("nova", "a", "style"),
                "a",
                2,
            )
            .unwrap()
            .unwrap();
        let b = store
            .record_owner_correction(
                "nova",
                UserModelKind::Preference,
                "B correction.",
                "style",
                &evidence("nova", "b", "style"),
                "b",
                3,
            )
            .unwrap()
            .unwrap();
        assert_ne!(a.semantic_key, b.semantic_key);
        for candidate in [&a, &b] {
            assert!(candidate.semantic_key.len() <= 64);
            store
                .review_candidate(
                    &candidate.id,
                    ReviewAction::Accept,
                    "operator",
                    None,
                    None,
                    4,
                )
                .unwrap();
        }
        let replacement = store
            .record_owner_correction(
                "nova",
                UserModelKind::Preference,
                "New A correction.",
                "style",
                &evidence("nova", "a", "style"),
                "a",
                5,
            )
            .unwrap()
            .unwrap();
        assert_eq!(replacement.semantic_key, a.semantic_key);
        store
            .review_candidate(
                &replacement.id,
                ReviewAction::Accept,
                "operator",
                None,
                None,
                6,
            )
            .unwrap();
        let heads = store.active_heads(Some(6)).unwrap();
        assert_eq!(heads.len(), 3);
        for (session, own, other) in [
            ("a", "New A correction.", "B correction."),
            ("b", "B correction.", "New A correction."),
        ] {
            let projection = project_applicable_heads(
                heads.clone(),
                &ApplicabilityContext::new("nova", "test", session),
                1200,
            );
            assert!(projection.prompt_section.contains("Global baseline."));
            assert!(projection.prompt_section.contains(own));
            assert!(!projection.prompt_section.contains(other));
        }
    }

    #[test]
    fn owner_correction_scope_cannot_expand_and_reflection_stays_global() {
        let dir = tempfile::tempdir().unwrap();
        let store = UserModelStore::open(dir.path()).unwrap();
        let correction = store
            .record_owner_correction(
                "nova",
                UserModelKind::Preference,
                "Short answers.",
                "answers.length",
                &evidence("nova", "session-a", "answers.length"),
                "session-a",
                1,
            )
            .unwrap()
            .unwrap();
        for scope in ["global", "session:session-b", "agent:nova"] {
            assert!(
                store
                    .review_candidate(
                        &correction.id,
                        ReviewAction::Narrow,
                        "operator",
                        None,
                        Some(scope),
                        2
                    )
                    .is_err()
            );
        }
        assert!(store.active_heads(Some(2)).unwrap().is_empty());
        store
            .review_candidate(
                &correction.id,
                ReviewAction::Accept,
                "operator",
                None,
                None,
                3,
            )
            .unwrap();
        assert_eq!(
            store.active_heads(Some(3)).unwrap()[0].scope,
            "session:session-a"
        );
        let reflection = store
            .record_reflection_observation(
                UserModelKind::Preference,
                "Detailed reviews.",
                "reviews.detail",
                "reflection evidence",
                4,
            )
            .unwrap()
            .unwrap();
        assert_eq!(reflection.scope, "global");
        store
            .review_candidate(
                &reflection.id,
                ReviewAction::Accept,
                "operator",
                None,
                None,
                5,
            )
            .unwrap();
        let heads = store.active_heads(Some(5)).unwrap();
        assert!(
            heads
                .iter()
                .any(|head| head.semantic_key == "reviews.detail" && head.scope == "global")
        );
    }

    const LEGACY_UNINDEXED_SCHEMA_SQL: &str = "
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA busy_timeout = 5000;
        CREATE TABLE IF NOT EXISTS user_model_candidates (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            statement TEXT NOT NULL,
            semantic_key TEXT NOT NULL,
            scope TEXT NOT NULL DEFAULT 'global',
            evidence TEXT NOT NULL DEFAULT '[]',
            created_at_unix INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS user_model_revisions (
            id TEXT PRIMARY KEY,
            semantic_key TEXT NOT NULL,
            kind TEXT NOT NULL,
            statement TEXT NOT NULL,
            scope TEXT NOT NULL DEFAULT 'global',
            authority TEXT NOT NULL,
            supersedes TEXT,
            valid_from_unix INTEGER NOT NULL,
            valid_until_unix INTEGER,
            source_candidate TEXT,
            created_at_unix INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS user_model_review_receipts (
            id TEXT PRIMARY KEY,
            candidate_id TEXT NOT NULL,
            action TEXT NOT NULL,
            reviewer TEXT NOT NULL,
            note TEXT,
            at_unix INTEGER NOT NULL
        );
    ";

    fn open_legacy_unindexed_connection(db_path: &Path) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute_batch(LEGACY_UNINDEXED_SCHEMA_SQL).unwrap();
        conn
    }

    fn has_composite_revisions_index(conn: &rusqlite::Connection) -> bool {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_user_model_revisions_key_created_valid'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        count == 1
    }

    fn explain_query_plan(
        conn: &rusqlite::Connection,
        sql: &str,
        bind_as_of_and_key: bool,
    ) -> Vec<String> {
        let explain_sql = format!("EXPLAIN QUERY PLAN {sql}");
        let mut stmt = conn.prepare(&explain_sql).unwrap();
        let map_row = |row: &rusqlite::Row<'_>| row.get::<_, String>(3);
        if bind_as_of_and_key {
            stmt.query_map(rusqlite::params![100_i64, Option::<&str>::None], map_row)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        } else {
            stmt.query_map(rusqlite::params!["k", 100_i64], map_row)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        }
    }

    struct ScaleMeasurement {
        heads: Vec<UserModelRevision>,
        fullscan_steps: i32,
        sort_ops: i32,
        vm_steps: i32,
        median_ms: f64,
    }

    fn measure_active_heads_scale(
        conn: &rusqlite::Connection,
        as_of: u64,
        runs: usize,
    ) -> ScaleMeasurement {
        let mut timings_ms = Vec::with_capacity(runs);
        let mut latest_heads = Vec::new();
        for _ in 0..runs {
            let started = std::time::Instant::now();
            latest_heads = active_heads_from_connection(conn, as_of, None).unwrap();
            timings_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
        }
        timings_ms.sort_by(f64::total_cmp);
        let median_ms = timings_ms[timings_ms.len() / 2];

        let mut stmt = conn.prepare(ACTIVE_HEADS_SQL).unwrap();
        stmt.reset_status(rusqlite::StatementStatus::FullscanStep);
        stmt.reset_status(rusqlite::StatementStatus::Sort);
        stmt.reset_status(rusqlite::StatementStatus::VmStep);
        let drained = stmt
            .query_map(
                rusqlite::params![as_of, Option::<&str>::None],
                revision_from_row,
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(drained.len() >= latest_heads.len());
        let fullscan_steps = stmt.get_status(rusqlite::StatementStatus::FullscanStep);
        let sort_ops = stmt.get_status(rusqlite::StatementStatus::Sort);
        let vm_steps = stmt.get_status(rusqlite::StatementStatus::VmStep);

        ScaleMeasurement {
            heads: latest_heads,
            fullscan_steps,
            sort_ops,
            vm_steps,
            median_ms,
        }
    }

    fn seed_deterministic_scale_dataset(conn: &rusqlite::Connection, total_revisions: usize) {
        assert_eq!(total_revisions % 5, 0);
        let num_keys = total_revisions / 5;
        let tx = conn.unchecked_transaction().unwrap();
        {
            let mut insert_cand = tx
                .prepare(
                    "INSERT INTO user_model_candidates
                         (id, kind, statement, semantic_key, scope, evidence, created_at_unix)
                     VALUES (?1, 'preference', ?2, ?3, ?4, ?5, ?6)",
                )
                .unwrap();
            let mut insert_rev = tx
                .prepare(
                    "INSERT INTO user_model_revisions
                         (id, semantic_key, kind, statement, scope, authority, supersedes,
                          valid_from_unix, valid_until_unix, source_candidate, created_at_unix)
                     VALUES (?1, ?2, 'preference', ?3, ?4, 'owner_ratified', ?5, ?6, ?7, ?8, ?9)",
                )
                .unwrap();

            for key_idx in 0..num_keys {
                let is_oc = key_idx % 10 == 0;
                let is_expired_head = key_idx % 10 == 1;
                let is_future_only = key_idx % 10 == 2;
                let semantic_key = if is_oc {
                    format!("oc.{key_idx:060x}")
                } else {
                    format!("scale.key.{key_idx:05}")
                };
                let scope = if is_oc { "session:s1" } else { "global" };
                let cand_id = format!("cand-{key_idx:05}");
                insert_cand
                    .execute(rusqlite::params![
                        cand_id,
                        format!("candidate {key_idx}"),
                        semantic_key,
                        scope,
                        evidence("nova", "s1", "style"),
                        10_i64,
                    ])
                    .unwrap();

                let mut prev_id: Option<String> = None;
                for rev_idx in 0..5 {
                    let rev_id =
                        format!("rev-{key_idx:05}-{rev_idx}-{}", (key_idx * 7 + rev_idx) % 9);
                    let created = if is_oc && rev_idx >= 3 {
                        50_i64
                    } else {
                        10_i64 + rev_idx as i64 * 10
                    };
                    let valid_from = if is_future_only && rev_idx >= 3 {
                        500_i64
                    } else {
                        created
                    };
                    let valid_until: Option<i64> = if is_expired_head && rev_idx == 4 {
                        Some(90)
                    } else {
                        None
                    };
                    insert_rev
                        .execute(rusqlite::params![
                            rev_id,
                            semantic_key,
                            format!("statement {key_idx}-{rev_idx}"),
                            scope,
                            prev_id,
                            valid_from,
                            valid_until,
                            cand_id,
                            created,
                        ])
                        .unwrap();
                    prev_id = Some(rev_id);
                }
            }
        }
        tx.commit().unwrap();
    }

    #[test]
    fn legacy_unindexed_db_migrates_reopens_and_preserves_projection_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("user_model.db");
        let legacy_conn = open_legacy_unindexed_connection(&db_path);
        assert!(
            !has_composite_revisions_index(&legacy_conn),
            "precondition: legacy DB has no composite index"
        );
        let legacy_store = UserModelStore {
            conn: Mutex::new(legacy_conn),
        };

        // 1. Global active revision + superseded history.
        legacy_store
            .record_owner_statement(
                UserModelKind::Preference,
                "Old global preference",
                "style.tone",
                "global",
                10,
            )
            .unwrap();
        legacy_store
            .record_owner_statement(
                UserModelKind::Preference,
                "Active global preference",
                "style.tone",
                "global",
                20,
            )
            .unwrap();

        // 2. Future-effective revision (must stay hidden at as_of=100, surface at as_of=200).
        legacy_store
            .record_owner_statement(
                UserModelKind::Goal,
                "Future goal",
                "goal.future",
                "global",
                200,
            )
            .unwrap();

        // 3. Newest revision expired -> must NOT resurrect older active revision at as_of=100.
        legacy_store
            .conn
            .lock()
            .execute(
                "INSERT INTO user_model_revisions
                     (id, semantic_key, kind, statement, scope, authority, supersedes,
                      valid_from_unix, valid_until_unix, source_candidate, created_at_unix)
                 VALUES
                     ('exp-old', 'exp.key', 'preference', 'Older active', 'global', 'owner_authored', NULL, 10, NULL, NULL, 10),
                     ('exp-new', 'exp.key', 'preference', 'Expired newest', 'global', 'owner_authored', 'exp-old', 20, 50, NULL, 20)",
                [],
            )
            .unwrap();

        // 4. Same-second oc.<60-hex> tie-breaking + cross-agent/session isolation.
        for (statement, forced_id) in [
            ("First tie", "zzzz"),
            ("Second tie", "aaaa"),
            ("Winner tie", "bbbb"),
        ] {
            let cand = legacy_store
                .record_owner_correction(
                    "nova",
                    UserModelKind::Preference,
                    statement,
                    "reply.style",
                    &evidence("nova", "session-a", "reply.style"),
                    "session-a",
                    30,
                )
                .unwrap()
                .unwrap();
            legacy_store
                .review_candidate(&cand.id, ReviewAction::Accept, "operator", None, None, 40)
                .unwrap();
            legacy_store
                .conn
                .lock()
                .execute(
                    "UPDATE user_model_revisions SET id = ?1 WHERE source_candidate = ?2",
                    rusqlite::params![forced_id, cand.id],
                )
                .unwrap();
        }
        let other_cand = legacy_store
            .record_owner_correction(
                "other-agent",
                UserModelKind::Preference,
                "Other agent correction",
                "reply.style",
                &evidence("other-agent", "session-b", "reply.style"),
                "session-b",
                30,
            )
            .unwrap()
            .unwrap();
        legacy_store
            .review_candidate(
                &other_cand.id,
                ReviewAction::Accept,
                "operator",
                None,
                None,
                40,
            )
            .unwrap();

        // Pending candidates left for concurrent multi-thread review after migration.
        let mut pending_ids = Vec::new();
        for idx in 0..4 {
            let cand = legacy_store
                .record_observation(
                    UserModelKind::Habit,
                    &format!("Concurrent observation {idx}"),
                    &format!("concurrent.key.{idx}"),
                    "[]",
                    50,
                )
                .unwrap();
            pending_ids.push(cand.id);
        }

        let baseline_100 = legacy_store.active_heads(Some(100)).unwrap();
        let baseline_200 = legacy_store.active_heads(Some(200)).unwrap();
        let baseline_nova = legacy_store
            .active_heads_for_agent("nova", Some(100))
            .unwrap();
        let baseline_other = legacy_store
            .active_heads_for_agent("other-agent", Some(100))
            .unwrap();
        drop(legacy_store);

        // Migrate once via UserModelStore::open, then reopen a second time to verify idempotency.
        let migrated = UserModelStore::open(dir.path()).unwrap();
        assert!(has_composite_revisions_index(&migrated.conn.lock()));
        drop(migrated);
        let reopened = std::sync::Arc::new(UserModelStore::open(dir.path()).unwrap());
        assert!(has_composite_revisions_index(&reopened.conn.lock()));

        assert_eq!(reopened.active_heads(Some(100)).unwrap(), baseline_100);
        assert_eq!(reopened.active_heads(Some(200)).unwrap(), baseline_200);
        assert_eq!(
            reopened.active_heads_for_agent("nova", Some(100)).unwrap(),
            baseline_nova
        );
        assert_eq!(
            reopened
                .active_heads_for_agent("other-agent", Some(100))
                .unwrap(),
            baseline_other
        );

        let nova_heads = reopened.active_heads_for_agent("nova", Some(100)).unwrap();
        assert!(nova_heads.iter().any(|h| h.statement == "Winner tie"));
        assert!(
            !nova_heads
                .iter()
                .any(|h| h.statement == "Other agent correction")
        );
        assert!(!nova_heads.iter().any(|h| h.semantic_key == "exp.key"));
        assert!(!nova_heads.iter().any(|h| h.semantic_key == "goal.future"));

        // Verify cross-session isolation through applicability projection.
        let proj_a = project_applicable_heads(
            nova_heads.clone(),
            &ApplicabilityContext::new("nova", "cli", "session-a"),
            1_200,
        );
        let proj_b = project_applicable_heads(
            nova_heads,
            &ApplicabilityContext::new("nova", "cli", "session-b"),
            1_200,
        );
        assert!(proj_a.prompt_section.contains("Winner tie"));
        assert!(!proj_b.prompt_section.contains("Winner tie"));

        // Concurrent reviews across threads on the migrated store must all commit cleanly.
        std::thread::scope(|scope| {
            for (idx, cid) in pending_ids.iter().enumerate() {
                let store_ref = std::sync::Arc::clone(&reopened);
                scope.spawn(move || {
                    store_ref
                        .review_candidate(
                            cid,
                            ReviewAction::Accept,
                            "operator",
                            None,
                            None,
                            110 + idx as u64,
                        )
                        .unwrap();
                });
            }
        });
        let after_concurrent = reopened.active_heads(Some(120)).unwrap();
        for idx in 0..4 {
            assert!(
                after_concurrent
                    .iter()
                    .any(|h| h.semantic_key == format!("concurrent.key.{idx}")),
                "concurrently reviewed candidate {idx} must be active"
            );
        }

        // Supersession chain on record_owner_statement still links to the active head.
        let next_global = reopened
            .record_owner_statement(
                UserModelKind::Preference,
                "Newest global preference",
                "style.tone",
                "global",
                130,
            )
            .unwrap();
        let prev_id = baseline_100
            .iter()
            .find(|h| h.semantic_key == "style.tone")
            .map(|h| h.id.as_str());
        assert_eq!(next_global.supersedes.as_deref(), prev_id);
    }

    #[test]
    fn query_plan_and_scale_datasets_preserve_equivalence_and_avoid_quadratic_scans() {
        for scale in [1_000_usize, 5_000, 10_000] {
            let unindexed_dir = tempfile::tempdir().unwrap();
            let unindexed_conn =
                open_legacy_unindexed_connection(&unindexed_dir.path().join("user_model.db"));
            assert!(!has_composite_revisions_index(&unindexed_conn));
            seed_deterministic_scale_dataset(&unindexed_conn, scale);

            let indexed_dir = tempfile::tempdir().unwrap();
            let indexed_store = UserModelStore::open(indexed_dir.path()).unwrap();
            let indexed_conn = indexed_store.conn.lock();
            assert!(has_composite_revisions_index(&indexed_conn));
            seed_deterministic_scale_dataset(&indexed_conn, scale);

            let unindexed = measure_active_heads_scale(&unindexed_conn, 100, 1);
            let indexed = measure_active_heads_scale(&indexed_conn, 100, 3);

            assert_eq!(
                indexed.heads, unindexed.heads,
                "scale={scale}: indexed query must return exact row-for-row equivalent active heads"
            );
            assert!(
                indexed.fullscan_steps * 10 < unindexed.fullscan_steps,
                "scale={scale}: indexed FullscanStep ({}) should be >10x smaller than unindexed ({})",
                indexed.fullscan_steps,
                unindexed.fullscan_steps
            );

            let heads_plan = explain_query_plan(&indexed_conn, ACTIVE_HEADS_SQL, true);
            assert!(
                heads_plan.iter().any(|line| {
                    line.contains("USING COVERING INDEX idx_user_model_revisions_key_created_valid")
                }),
                "scale={scale}: expected active_heads correlated subqueries to use covering index, got: {heads_plan:?}"
            );
            let owner_lookup_plan =
                explain_query_plan(&indexed_conn, OWNER_STATEMENT_SUPERSEDES_SQL, false);
            assert!(
                owner_lookup_plan.iter().any(|line| {
                    line.contains("USING INDEX idx_user_model_revisions_key_created_valid")
                }),
                "scale={scale}: expected owner lookup to use composite index, got: {owner_lookup_plan:?}"
            );
            assert!(
                !owner_lookup_plan
                    .iter()
                    .any(|line| line.contains("SCAN user_model_revisions")),
                "scale={scale}: owner lookup must not table-scan user_model_revisions: {owner_lookup_plan:?}"
            );

            println!(
                "user_model_scale={scale} rows: heads={} unindexed(1x)={:.3}ms fullscan={} sort={} vm={} | indexed(3x-med)={:.3}ms fullscan={} sort={} vm={}",
                unindexed.heads.len(),
                unindexed.median_ms,
                unindexed.fullscan_steps,
                unindexed.sort_ops,
                unindexed.vm_steps,
                indexed.median_ms,
                indexed.fullscan_steps,
                indexed.sort_ops,
                indexed.vm_steps,
            );
            if scale == 10_000 {
                println!("active_heads_eqp={heads_plan:?}");
                println!("owner_lookup_eqp={owner_lookup_plan:?}");
            }
        }
    }
}
