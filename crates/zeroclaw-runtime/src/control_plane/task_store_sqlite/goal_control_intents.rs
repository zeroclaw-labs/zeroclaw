//! Audit storage for normal Goal controls. Task rows remain lifecycle authority.

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use zeroclaw_commands::goal::{
    GoalBudgetSelection, is_valid_finite_goal_cost_limit, is_valid_finite_goal_token_limit,
};
use zeroclaw_goal::task::{
    GoalControlIntent, GoalControlIntentKind, GoalControlIntentSettlement, GoalTransitionResult,
};

const MAX_REASON_BYTES: usize = 64;

pub(super) fn converge_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS goal_control_intents (
             intent_id TEXT PRIMARY KEY NOT NULL CHECK(length(trim(intent_id)) > 0),
             session_id TEXT NOT NULL CHECK(length(trim(session_id)) > 0),
             task_id TEXT REFERENCES tasks(id) ON DELETE CASCADE
                 CHECK(task_id IS NULL OR length(trim(task_id)) > 0),
             expected_epoch INTEGER,
             predecessor_task_id TEXT
                 CHECK(predecessor_task_id IS NULL OR length(trim(predecessor_task_id)) > 0),
             kind TEXT NOT NULL CHECK(kind IN ('start','resume','pause','set_budget')),
             parameters_json TEXT NOT NULL DEFAULT '{}'
                 CHECK(length(parameters_json) <= 512 AND json_valid(parameters_json)),
             state TEXT NOT NULL
                 CHECK(state IN ('pending','claimed','applied','superseded','abandoned')),
             outcome_reason TEXT CHECK(outcome_reason IS NULL OR
                 (length(outcome_reason) BETWEEN 1 AND 64
                  AND outcome_reason NOT GLOB '*[^a-z0-9_]*')),
             owner_pid INTEGER NOT NULL CHECK(owner_pid BETWEEN 1 AND 4294967295),
             owner_boot_id TEXT NOT NULL CHECK(length(trim(owner_boot_id)) > 0),
             created_at TEXT NOT NULL CHECK(length(trim(created_at)) > 0),
             claimed_at TEXT,
             settled_at TEXT,
             CHECK((kind = 'start' AND task_id IS NULL AND expected_epoch IS NULL)
                OR (kind != 'start' AND task_id IS NOT NULL
                    AND typeof(expected_epoch) = 'integer' AND expected_epoch >= 1)),
             CHECK(kind = 'start' OR predecessor_task_id IS NULL)
         );
         CREATE INDEX IF NOT EXISTS idx_goal_control_intents_session_state
             ON goal_control_intents(session_id, state);
         CREATE INDEX IF NOT EXISTS idx_goal_control_intents_boot
             ON goal_control_intents(owner_boot_id);",
    )
    .context("ensure Goal control intent audit schema")
}

// Only these typed parameters can be persisted. The command's free text has no
// field in this projection and must never be copied into an audit envelope.
#[derive(Serialize)]
struct IntentParameters {
    #[serde(skip_serializing_if = "Option::is_none")]
    budget: Option<IntentBudget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    has_response: Option<bool>,
}

#[derive(Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum IntentBudget {
    Defaults,
    Unlimited,
    Limits {
        token_limit: Option<u64>,
        cost_limit_usd: Option<f64>,
    },
}

fn budget_parameters(budget: GoalBudgetSelection, allow_defaults: bool) -> Result<IntentBudget> {
    match budget {
        GoalBudgetSelection::Defaults => {
            ensure!(allow_defaults, "Goal budget update cannot select defaults");
            Ok(IntentBudget::Defaults)
        }
        GoalBudgetSelection::Unlimited => Ok(IntentBudget::Unlimited),
        GoalBudgetSelection::Limits(limits) => {
            ensure!(
                limits
                    .token_limit
                    .is_none_or(is_valid_finite_goal_token_limit)
                    && limits
                        .cost_limit_usd
                        .is_none_or(is_valid_finite_goal_cost_limit),
                "Goal control intent has invalid budget limits"
            );
            Ok(IntentBudget::Limits {
                token_limit: limits.token_limit,
                cost_limit_usd: limits.cost_limit_usd,
            })
        }
    }
}

fn parameters(kind: &GoalControlIntentKind) -> Result<(&'static str, String)> {
    let (name, budget, has_response) = match kind {
        GoalControlIntentKind::Start { budget } => {
            ("start", Some(budget_parameters(*budget, true)?), None)
        }
        GoalControlIntentKind::SetBudget { budget } => {
            ("set_budget", Some(budget_parameters(*budget, false)?), None)
        }
        GoalControlIntentKind::Resume { has_response } => ("resume", None, Some(*has_response)),
        GoalControlIntentKind::Pause => ("pause", None, None),
    };
    let json = serde_json::to_string(&IntentParameters {
        budget,
        has_response,
    })
    .context("serialize bounded Goal control intent parameters")?;
    Ok((name, json))
}

pub(super) fn record(conn: &Connection, intent: &GoalControlIntent) -> Result<()> {
    let uuid =
        uuid::Uuid::parse_str(&intent.intent_id).context("invalid Goal control intent UUID")?;
    ensure!(
        uuid.get_version() == Some(uuid::Version::Random),
        "Goal control intent requires UUID v4"
    );
    ensure!(
        !intent.session_id.trim().is_empty()
            && !intent.owner_boot_id.trim().is_empty()
            && !intent.created_at.trim().is_empty()
            && intent
                .task_id
                .as_ref()
                .is_none_or(|id| !id.trim().is_empty())
            && intent
                .predecessor_task_id
                .as_ref()
                .is_none_or(|id| !id.trim().is_empty()),
        "Goal control intent requires nonblank binding and ownership"
    );
    let (kind, parameters_json) = parameters(&intent.kind)?;
    conn.execute(
        "INSERT INTO goal_control_intents
             (intent_id, session_id, task_id, expected_epoch, predecessor_task_id,
              kind, parameters_json, state, owner_pid, owner_boot_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9, ?10)",
        params![
            intent.intent_id,
            intent.session_id,
            intent.task_id,
            intent.expected_epoch,
            intent.predecessor_task_id,
            kind,
            parameters_json,
            intent.owner_pid,
            intent.owner_boot_id,
            intent.created_at
        ],
    )
    .context("record pending Goal control intent")?;
    Ok(())
}

fn result(conn: &Connection, intent_id: &str, changed: usize) -> Result<GoalTransitionResult> {
    if changed == 1 {
        return Ok(GoalTransitionResult::Applied);
    }
    let exists = conn
        .query_row(
            "SELECT 1 FROM goal_control_intents WHERE intent_id = ?1",
            params![intent_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .context("classify guarded Goal intent transition")?;
    Ok(if exists.is_some() {
        GoalTransitionResult::Stale
    } else {
        GoalTransitionResult::Missing
    })
}

pub(super) fn claim(
    conn: &Connection,
    intent_id: &str,
    boot_id: &str,
) -> Result<GoalTransitionResult> {
    let changed = conn
        .execute(
            "UPDATE goal_control_intents SET state = 'claimed', claimed_at = ?3
         WHERE intent_id = ?1 AND owner_boot_id = ?2 AND state = 'pending'",
            params![intent_id, boot_id, chrono::Utc::now().to_rfc3339()],
        )
        .context("claim pending Goal control intent")?;
    result(conn, intent_id, changed)
}

pub(super) fn settle(
    conn: &Connection,
    intent_id: &str,
    settlement: GoalControlIntentSettlement,
) -> Result<GoalTransitionResult> {
    let (state, reason) = match settlement {
        GoalControlIntentSettlement::Applied => ("applied", None),
        GoalControlIntentSettlement::Superseded { reason } => ("superseded", Some(reason)),
        GoalControlIntentSettlement::Abandoned { reason } => ("abandoned", Some(reason)),
    };
    if let Some(reason) = reason.as_ref() {
        ensure!(
            !reason.is_empty()
                && reason.len() <= MAX_REASON_BYTES
                && reason
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
            "Goal control intent outcome requires a bounded token"
        );
    }
    let changed = conn
        .execute(
            "UPDATE goal_control_intents SET state = ?2, outcome_reason = ?3, settled_at = ?4
         WHERE intent_id = ?1
           AND (state = 'claimed' OR (state = 'pending' AND ?2 != 'applied'))",
            params![intent_id, state, reason, chrono::Utc::now().to_rfc3339()],
        )
        .context("settle Goal control intent")?;
    result(conn, intent_id, changed)
}

pub(super) fn abandon_previous_boot(conn: &Connection, boot_id: &str, now: &str) -> Result<()> {
    conn.execute(
        "UPDATE goal_control_intents
            SET state = 'abandoned', outcome_reason = 'restart', settled_at = ?2
         WHERE owner_boot_id != ?1 AND state IN ('pending','claimed')",
        params![boot_id, now],
    )
    .context("abandon previous-boot Goal control intents")?;
    Ok(())
}

pub(super) fn delete_settled_start_intents(conn: &Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM goal_control_intents WHERE session_id = ?1 AND kind = 'start'
         AND state IN ('applied','superseded','abandoned')",
        params![session_id],
    )
    .context("delete settled Start intents for disposed Goal session")?;
    Ok(())
}
