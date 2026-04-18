// Workflow Learning Loop (v3.0 Section S4+)
//
// Analyzes workflow_runs to generate improvement suggestions.
// Runs as a subtask of Dream Cycle (per plan §S4 task #5).
//
// Three suggestion types:
//   - fix_failure:    failure rate > 20% → LLM analyzes common errors
//   - default_value:  repeated param value (mode freq > 70%) → update default
//   - abstraction:    3+ similar workflows → parameterize to higher abstraction

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::params;

/// A single workflow run record relevant for analysis.
#[derive(Debug, Clone)]
pub struct WorkflowRunRecord {
    pub run_uuid: String,
    pub workflow_id: i64,
    pub status: String,
    pub input_json: Option<String>,
    pub error_message: Option<String>,
    pub cost_tokens_in: u32,
    pub cost_tokens_out: u32,
    pub cost_llm_calls: u32,
}

/// Input for the learning loop: a workflow + its recent runs.
#[derive(Debug, Clone)]
pub struct WorkflowAnalysisInput {
    pub workflow_id: i64,
    pub workflow_name: String,
    pub usage_count: u32,
    pub runs: Vec<WorkflowRunRecord>,
}

/// A suggestion produced by the learning loop.
#[derive(Debug, Clone)]
pub struct WorkflowSuggestion {
    pub workflow_id: Option<i64>,
    pub suggestion_type: SuggestionType,
    pub title: String,
    pub description: String,
    pub patch_yaml: Option<String>,
}

/// Types of improvement suggestions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionType {
    FixFailure,
    DefaultValue,
    Abstraction,
    Deprecation,
}

impl SuggestionType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FixFailure => "fix_failure",
            Self::DefaultValue => "default_value",
            Self::Abstraction => "abstraction",
            Self::Deprecation => "deprecation",
        }
    }
}

/// Configuration for the learning analysis.
#[derive(Debug, Clone)]
pub struct LearningConfig {
    /// Minimum usage count to consider a workflow for analysis.
    pub min_usage: u32,
    /// Failure rate threshold above which to emit fix_failure suggestions.
    pub failure_threshold: f32,
    /// Mode frequency threshold for default_value suggestions.
    pub default_value_threshold: f32,
    /// Minimum similar workflows to trigger abstraction suggestion.
    pub abstraction_min_count: usize,
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            min_usage: 5,
            failure_threshold: 0.2,
            default_value_threshold: 0.7,
            abstraction_min_count: 3,
        }
    }
}

/// Analyze a single workflow's runs and emit suggestions.
pub fn analyze_workflow(
    input: &WorkflowAnalysisInput,
    config: &LearningConfig,
) -> Vec<WorkflowSuggestion> {
    let mut suggestions = Vec::new();

    if input.usage_count < config.min_usage {
        return suggestions;
    }

    // 1. Failure rate analysis
    if let Some(s) = analyze_failures(input, config) {
        suggestions.push(s);
    }

    // 2. Repeated parameter detection
    suggestions.extend(analyze_param_frequency(input, config));

    suggestions
}

/// Emit a fix_failure suggestion if the failure rate is above threshold.
fn analyze_failures(
    input: &WorkflowAnalysisInput,
    config: &LearningConfig,
) -> Option<WorkflowSuggestion> {
    if input.runs.is_empty() {
        return None;
    }

    let failed_count = input
        .runs
        .iter()
        .filter(|r| r.status == "failed")
        .count();
    let total = input.runs.len();
    let failure_rate = failed_count as f32 / total as f32;

    if failure_rate < config.failure_threshold {
        return None;
    }

    // Extract common error patterns
    let mut error_counts: HashMap<String, usize> = HashMap::new();
    for run in &input.runs {
        if let Some(ref err) = run.error_message {
            // Truncate to first 80 chars for grouping
            let key = err.chars().take(80).collect::<String>();
            *error_counts.entry(key).or_insert(0) += 1;
        }
    }

    let top_errors: Vec<(String, usize)> = {
        let mut errs: Vec<(String, usize)> = error_counts.into_iter().collect();
        errs.sort_by(|a, b| b.1.cmp(&a.1));
        errs.into_iter().take(3).collect()
    };

    let error_summary = if top_errors.is_empty() {
        "(error messages not captured)".to_string()
    } else {
        top_errors
            .iter()
            .map(|(e, c)| format!("  • [{c}회] {e}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    Some(WorkflowSuggestion {
        workflow_id: Some(input.workflow_id),
        suggestion_type: SuggestionType::FixFailure,
        title: format!(
            "'{}' 실패율 높음 ({:.0}%)",
            input.workflow_name,
            failure_rate * 100.0
        ),
        description: format!(
            "최근 {total}회 실행 중 {failed_count}회 실패. 주요 에러:\n{error_summary}"
        ),
        patch_yaml: None, // Patch generation would require LLM call
    })
}

/// Detect parameters where a single value dominates (mode frequency > threshold).
fn analyze_param_frequency(
    input: &WorkflowAnalysisInput,
    config: &LearningConfig,
) -> Vec<WorkflowSuggestion> {
    // Collect all input values, grouped by parameter name
    let mut param_values: HashMap<String, Vec<String>> = HashMap::new();

    for run in &input.runs {
        if let Some(ref input_json) = run.input_json {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(input_json) {
                if let Some(obj) = value.as_object() {
                    for (k, v) in obj {
                        let val_str = match v {
                            serde_json::Value::String(s) => s.clone(),
                            _ => v.to_string(),
                        };
                        param_values.entry(k.clone()).or_default().push(val_str);
                    }
                }
            }
        }
    }

    let mut suggestions = Vec::new();
    for (param, values) in param_values {
        let total = values.len();
        if total < 3 {
            continue;
        }

        // Find mode (most frequent value)
        let mut counts: HashMap<String, usize> = HashMap::new();
        for v in &values {
            *counts.entry(v.clone()).or_insert(0) += 1;
        }
        if let Some((mode_val, mode_count)) = counts.iter().max_by_key(|(_, c)| *c) {
            let frequency = *mode_count as f32 / total as f32;
            if frequency >= config.default_value_threshold {
                suggestions.push(WorkflowSuggestion {
                    workflow_id: Some(input.workflow_id),
                    suggestion_type: SuggestionType::DefaultValue,
                    title: format!(
                        "'{}' 입력 '{}' 의 기본값을 '{}'로 설정",
                        input.workflow_name, param, mode_val
                    ),
                    description: format!(
                        "최근 {total}회 중 {mode_count}회 ({:.0}%) 같은 값 사용 — 기본값 제안",
                        frequency * 100.0
                    ),
                    patch_yaml: Some(format!("inputs.{param}.default = {mode_val:?}")),
                });
            }
        }
    }

    suggestions
}

/// Load recent runs for a workflow from SQLite.
pub fn load_recent_runs(
    conn: &rusqlite::Connection,
    workflow_id: i64,
    limit: usize,
) -> Result<Vec<WorkflowRunRecord>> {
    let mut stmt = conn.prepare(
        "SELECT uuid, workflow_id, status, input_json, error_message,
                cost_tokens_in, cost_tokens_out, cost_llm_calls
         FROM workflow_runs
         WHERE workflow_id = ?1
         ORDER BY started_at DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![workflow_id, limit as i64], |row| {
        // SQLite stores these counters as INTEGER; writers (`save_run_result`
        // below) come from u32 fields on WorkflowRunRecord. `try_from` on a
        // clamped-non-negative i64 saturates both signs of the unlikely
        // corrupt-row path (negative → 0 via `max(0)`, > u32::MAX → u32::MAX).
        let tokens_in = u32::try_from(row.get::<_, i64>(5)?.max(0)).unwrap_or(u32::MAX);
        let tokens_out = u32::try_from(row.get::<_, i64>(6)?.max(0)).unwrap_or(u32::MAX);
        let llm_calls = u32::try_from(row.get::<_, i64>(7)?.max(0)).unwrap_or(u32::MAX);
        Ok(WorkflowRunRecord {
            run_uuid: row.get(0)?,
            workflow_id: row.get(1)?,
            status: row.get(2)?,
            input_json: row.get(3)?,
            error_message: row.get(4)?,
            cost_tokens_in: tokens_in,
            cost_tokens_out: tokens_out,
            cost_llm_calls: llm_calls,
        })
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Load workflows with at least `min_usage` uses.
pub fn load_active_workflows(
    conn: &rusqlite::Connection,
    min_usage: u32,
) -> Result<Vec<(i64, String, u32)>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, usage_count FROM workflows
         WHERE usage_count >= ?1 AND is_archived = 0
         ORDER BY usage_count DESC",
    )?;
    let rows = stmt.query_map(params![min_usage], |row| {
        // `usage_count` is written as u32; saturating try_from mirrors `load_recent_runs`.
        let usage_count = u32::try_from(row.get::<_, i64>(2)?.max(0)).unwrap_or(u32::MAX);
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, usage_count))
    })?;
    let mut results = Vec::new();
    for row in rows {
        results.push(row?);
    }
    Ok(results)
}

/// Persist a suggestion to the workflow_suggestions table.
pub fn save_suggestion(
    conn: &rusqlite::Connection,
    suggestion: &WorkflowSuggestion,
) -> Result<i64> {
    let uuid = uuid::Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    conn.execute(
        "INSERT INTO workflow_suggestions
            (uuid, workflow_id, suggestion_type, title, description, patch_yaml, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            uuid,
            suggestion.workflow_id,
            suggestion.suggestion_type.as_str(),
            suggestion.title,
            suggestion.description,
            suggestion.patch_yaml,
            now,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Run the full learning loop: load workflows → analyze → save suggestions.
pub fn run_learning_loop(
    conn: &rusqlite::Connection,
    config: &LearningConfig,
) -> Result<usize> {
    let workflows = load_active_workflows(conn, config.min_usage)?;
    let mut saved = 0;

    for (id, name, usage_count) in workflows {
        let runs = load_recent_runs(conn, id, 50)?;
        let input = WorkflowAnalysisInput {
            workflow_id: id,
            workflow_name: name,
            usage_count,
            runs,
        };
        let suggestions = analyze_workflow(&input, config);
        for s in suggestions {
            if save_suggestion(conn, &s).is_ok() {
                saved += 1;
            }
        }
    }

    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_schema(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE workflows (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                uuid TEXT NOT NULL UNIQUE,
                parent_category TEXT NOT NULL,
                name TEXT NOT NULL,
                description TEXT,
                icon TEXT,
                spec_yaml TEXT NOT NULL,
                spec_sha256 TEXT NOT NULL,
                trigger_type TEXT NOT NULL DEFAULT 'manual',
                trigger_config_json TEXT,
                version INTEGER NOT NULL DEFAULT 1,
                parent_workflow_id INTEGER,
                created_by TEXT NOT NULL DEFAULT 'user',
                usage_count INTEGER NOT NULL DEFAULT 0,
                last_used_at INTEGER,
                is_pinned INTEGER NOT NULL DEFAULT 0,
                is_archived INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE workflow_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                uuid TEXT NOT NULL UNIQUE,
                workflow_id INTEGER NOT NULL,
                workflow_version INTEGER NOT NULL DEFAULT 1,
                started_at INTEGER NOT NULL,
                ended_at INTEGER,
                status TEXT NOT NULL,
                trigger_source TEXT,
                input_json TEXT,
                input_sha256 TEXT,
                output_ref TEXT,
                output_sha256 TEXT,
                error_message TEXT,
                cost_tokens_in INTEGER DEFAULT 0,
                cost_tokens_out INTEGER DEFAULT 0,
                cost_llm_calls INTEGER DEFAULT 0,
                feedback_rating INTEGER,
                feedback_note TEXT,
                device_id TEXT NOT NULL
            );
            CREATE TABLE workflow_suggestions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                uuid TEXT NOT NULL UNIQUE,
                workflow_id INTEGER,
                suggestion_type TEXT NOT NULL,
                title TEXT NOT NULL,
                description TEXT NOT NULL,
                patch_yaml TEXT,
                created_at INTEGER NOT NULL,
                reviewed_at INTEGER,
                review_decision TEXT
            );",
        )
        .unwrap();
    }

    fn make_run(workflow_id: i64, status: &str, input_json: Option<&str>, error: Option<&str>) -> WorkflowRunRecord {
        WorkflowRunRecord {
            run_uuid: uuid::Uuid::new_v4().to_string(),
            workflow_id,
            status: status.to_string(),
            input_json: input_json.map(String::from),
            error_message: error.map(String::from),
            cost_tokens_in: 100,
            cost_tokens_out: 200,
            cost_llm_calls: 1,
        }
    }

    #[test]
    fn default_config() {
        let c = LearningConfig::default();
        assert_eq!(c.min_usage, 5);
        assert_eq!(c.failure_threshold, 0.2);
    }

    #[test]
    fn skip_workflows_below_min_usage() {
        let input = WorkflowAnalysisInput {
            workflow_id: 1,
            workflow_name: "test".to_string(),
            usage_count: 3, // below default min_usage of 5
            runs: vec![make_run(1, "failed", None, None)],
        };
        let config = LearningConfig::default();
        let suggestions = analyze_workflow(&input, &config);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn emit_fix_failure_when_high_failure_rate() {
        let runs = vec![
            make_run(1, "failed", None, Some("connection timeout")),
            make_run(1, "failed", None, Some("connection timeout")),
            make_run(1, "failed", None, Some("auth error")),
            make_run(1, "success", None, None),
            make_run(1, "success", None, None),
        ];
        let input = WorkflowAnalysisInput {
            workflow_id: 1,
            workflow_name: "test_wf".to_string(),
            usage_count: 10,
            runs,
        };
        let suggestions = analyze_workflow(&input, &LearningConfig::default());
        assert!(suggestions.iter().any(|s| matches!(s.suggestion_type, SuggestionType::FixFailure)));
    }

    #[test]
    fn no_fix_failure_when_low_failure_rate() {
        // 1 out of 10 = 10% failure rate, below 20% threshold
        let mut runs = Vec::new();
        for _ in 0..9 {
            runs.push(make_run(1, "success", None, None));
        }
        runs.push(make_run(1, "failed", None, Some("timeout")));
        let input = WorkflowAnalysisInput {
            workflow_id: 1,
            workflow_name: "test_wf".to_string(),
            usage_count: 10,
            runs,
        };
        let failures: Vec<_> = analyze_workflow(&input, &LearningConfig::default())
            .into_iter()
            .filter(|s| matches!(s.suggestion_type, SuggestionType::FixFailure))
            .collect();
        assert!(failures.is_empty());
    }

    #[test]
    fn emit_default_value_for_repeated_param() {
        // 8 out of 10 runs use the same value for `category`
        let mut runs = Vec::new();
        for _ in 0..8 {
            runs.push(make_run(
                1,
                "success",
                Some(r#"{"category":"daily"}"#),
                None,
            ));
        }
        runs.push(make_run(1, "success", Some(r#"{"category":"phone"}"#), None));
        runs.push(make_run(1, "success", Some(r#"{"category":"other"}"#), None));

        let input = WorkflowAnalysisInput {
            workflow_id: 1,
            workflow_name: "test_wf".to_string(),
            usage_count: 10,
            runs,
        };
        let suggestions = analyze_workflow(&input, &LearningConfig::default());
        let defaults: Vec<_> = suggestions
            .iter()
            .filter(|s| matches!(s.suggestion_type, SuggestionType::DefaultValue))
            .collect();
        assert!(!defaults.is_empty(), "expected default_value suggestion for repeated 'category'");
        assert!(defaults[0].description.contains("80%"));
    }

    #[test]
    fn no_default_value_when_params_vary() {
        let runs: Vec<_> = (0..10)
            .map(|i| {
                make_run(
                    1,
                    "success",
                    Some(&format!(r#"{{"category":"val_{i}"}}"#)),
                    None,
                )
            })
            .collect();
        let input = WorkflowAnalysisInput {
            workflow_id: 1,
            workflow_name: "test_wf".to_string(),
            usage_count: 10,
            runs,
        };
        let suggestions = analyze_workflow(&input, &LearningConfig::default());
        let defaults: Vec<_> = suggestions
            .iter()
            .filter(|s| matches!(s.suggestion_type, SuggestionType::DefaultValue))
            .collect();
        assert!(defaults.is_empty());
    }

    #[test]
    fn save_and_load_suggestion_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        setup_schema(&conn);

        let suggestion = WorkflowSuggestion {
            workflow_id: Some(42),
            suggestion_type: SuggestionType::FixFailure,
            title: "test suggestion".to_string(),
            description: "test desc".to_string(),
            patch_yaml: Some("key: value".to_string()),
        };
        let id = save_suggestion(&conn, &suggestion).unwrap();
        assert!(id > 0);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM workflow_suggestions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn full_learning_loop_end_to_end() {
        let conn = Connection::open_in_memory().unwrap();
        setup_schema(&conn);
        let now = 1700000000i64;

        // Insert a workflow with usage_count = 10
        conn.execute(
            "INSERT INTO workflows
                (uuid, parent_category, name, spec_yaml, spec_sha256, usage_count, created_at, updated_at)
             VALUES ('wf-1', 'daily', 'test wf', 'name: t', 'sha', 10, ?1, ?1)",
            params![now],
        ).unwrap();
        let wf_id = conn.last_insert_rowid();

        // Insert 10 runs: 4 failed, 6 success
        for i in 0..10 {
            let status = if i < 4 { "failed" } else { "success" };
            let err = if i < 4 { Some("timeout".to_string()) } else { None };
            conn.execute(
                "INSERT INTO workflow_runs
                    (uuid, workflow_id, started_at, status, error_message, device_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'dev1')",
                params![format!("run-{i}"), wf_id, now + i * 100, status, err],
            ).unwrap();
        }

        let saved = run_learning_loop(&conn, &LearningConfig::default()).unwrap();
        assert!(saved > 0);

        let sug_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM workflow_suggestions WHERE workflow_id = ?1",
                params![wf_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(sug_count > 0);
    }

    #[test]
    fn no_runs_no_suggestions() {
        let input = WorkflowAnalysisInput {
            workflow_id: 1,
            workflow_name: "empty".to_string(),
            usage_count: 10,
            runs: Vec::new(),
        };
        let suggestions = analyze_workflow(&input, &LearningConfig::default());
        assert!(suggestions.is_empty());
    }

    #[test]
    fn suggestion_type_str() {
        assert_eq!(SuggestionType::FixFailure.as_str(), "fix_failure");
        assert_eq!(SuggestionType::DefaultValue.as_str(), "default_value");
        assert_eq!(SuggestionType::Abstraction.as_str(), "abstraction");
        assert_eq!(SuggestionType::Deprecation.as_str(), "deprecation");
    }
}
