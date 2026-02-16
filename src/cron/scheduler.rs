use crate::config::Config;
use crate::cron::{due_jobs, reschedule_after_run, CronJob};
use crate::feed::executor::FeedExecutor;
use crate::feed::scheduler::FeedScheduler;
use crate::memory;
use crate::providers;
use crate::security::SecurityPolicy;
use crate::status_events;
use crate::{agent, aria};
use anyhow::Result;
use chrono::Utc;
use rusqlite::params;
use serde_json::Value;
use std::sync::Arc;
use tokio::process::Command;
use tokio::time::{self, Duration};
use uuid::Uuid;

const MIN_POLL_SECONDS: u64 = 5;
const HEARTBEAT_QUEUE_PREFIX: &str = "[[AFW_QUEUE]] ";
const INTERNAL_FEED_COMMAND_PREFIX: &str = "__ARIA_FEED_EXEC__:";

#[derive(Debug, Clone)]
struct JobRunOutcome {
    success: bool,
    output: String,
    tenant_id: Option<String>,
    remove_after_run: bool,
    run_id: Option<String>,
}

pub async fn run(config: Config) -> Result<()> {
    let poll_secs = config.reliability.scheduler_poll_secs.max(MIN_POLL_SECONDS);
    let mut interval = time::interval(Duration::from_secs(poll_secs));
    let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

    crate::health::mark_component_ok("scheduler");

    loop {
        interval.tick().await;

        let jobs = match due_jobs(&config, Utc::now()) {
            Ok(jobs) => jobs,
            Err(e) => {
                crate::health::mark_component_error("scheduler", e.to_string());
                tracing::warn!("Scheduler query failed: {e}");
                continue;
            }
        };

        for job in jobs {
            crate::health::mark_component_ok("scheduler");
            let outcome = execute_job_with_retry(&config, &security, &job).await;
            let tenant = outcome
                .tenant_id
                .clone()
                .unwrap_or_else(|| "dev-tenant".to_string());
            let full_output = outcome.output.clone();
            if outcome.success {
                status_events::emit(
                    "cron.completed",
                    serde_json::json!({
                        "jobId": job.id,
                        "summary": outcome.output.lines().next().unwrap_or("ok"),
                        "fullOutput": full_output,
                        "runId": outcome.run_id,
                        "tenantId": tenant,
                    }),
                );
            } else {
                status_events::emit(
                    "cron.failed",
                    serde_json::json!({
                        "jobId": job.id,
                        "error": outcome.output,
                        "fullOutput": full_output,
                        "runId": outcome.run_id,
                        "tenantId": tenant,
                    }),
                );
            }

            if !outcome.success {
                crate::health::mark_component_error("scheduler", format!("job {} failed", job.id));
            }

            if outcome.remove_after_run {
                if let Err(e) = crate::cron::remove_job(&config, &job.id) {
                    crate::health::mark_component_error("scheduler", e.to_string());
                    tracing::warn!("Failed to remove one-shot cron job {}: {e}", job.id);
                }
            } else if let Err(e) =
                reschedule_after_run(&config, &job, outcome.success, &outcome.output)
            {
                crate::health::mark_component_error("scheduler", e.to_string());
                tracing::warn!("Failed to persist scheduler run result: {e}");
            }
        }
    }
}

async fn execute_job_with_retry(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
) -> JobRunOutcome {
    let mut last = JobRunOutcome {
        success: false,
        output: String::new(),
        tenant_id: None,
        remove_after_run: false,
        run_id: None,
    };
    let retries = config.reliability.scheduler_retries;
    let mut backoff_ms = config.reliability.provider_backoff_ms.max(200);

    for attempt in 0..=retries {
        let outcome = run_job_command(config, security, job).await;
        last = outcome.clone();

        if outcome.success || outcome.remove_after_run {
            return outcome;
        }

        if outcome.output.starts_with("blocked by security policy:") {
            // Deterministic policy violations are not retryable.
            return outcome;
        }

        if attempt < retries {
            let jitter_ms = u64::from(Utc::now().timestamp_subsec_millis() % 250);
            time::sleep(Duration::from_millis(backoff_ms + jitter_ms)).await;
            backoff_ms = (backoff_ms.saturating_mul(2)).min(30_000);
        }
    }

    last
}

fn is_env_assignment(word: &str) -> bool {
    word.contains('=')
        && word
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
}

fn strip_wrapping_quotes(token: &str) -> &str {
    token.trim_matches(|c| c == '"' || c == '\'')
}

fn forbidden_path_argument(security: &SecurityPolicy, command: &str) -> Option<String> {
    let mut normalized = command.to_string();
    for sep in ["&&", "||"] {
        normalized = normalized.replace(sep, "\x00");
    }
    for sep in ['\n', ';', '|'] {
        normalized = normalized.replace(sep, "\x00");
    }

    for segment in normalized.split('\x00') {
        let tokens: Vec<&str> = segment.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }

        // Skip leading env assignments and executable token.
        let mut idx = 0;
        while idx < tokens.len() && is_env_assignment(tokens[idx]) {
            idx += 1;
        }
        if idx >= tokens.len() {
            continue;
        }
        idx += 1;

        for token in &tokens[idx..] {
            let candidate = strip_wrapping_quotes(token);
            if candidate.is_empty() || candidate.starts_with('-') || candidate.contains("://") {
                continue;
            }

            let looks_like_path = candidate.starts_with('/')
                || candidate.starts_with("./")
                || candidate.starts_with("../")
                || candidate.starts_with("~/")
                || candidate.contains('/');

            if looks_like_path && !security.is_path_allowed(candidate) {
                return Some(candidate.to_string());
            }
        }
    }

    None
}

async fn run_job_command(
    config: &Config,
    security: &SecurityPolicy,
    job: &CronJob,
) -> JobRunOutcome {
    if let Some(cron_func_id) = job
        .command
        .strip_prefix(aria::cron_bridge::ARIA_CRON_COMMAND_PREFIX)
    {
        return run_aria_cron_function(config, cron_func_id.trim()).await;
    }
    if let Some(feed_id) = parse_feed_execution_command(&job.command) {
        return run_internal_feed_execution(config, &feed_id).await;
    }

    if !security.is_command_allowed(&job.command) {
        return JobRunOutcome {
            success: false,
            output: format!(
                "blocked by security policy: command not allowed: {}",
                job.command
            ),
            tenant_id: None,
            remove_after_run: false,
            run_id: None,
        };
    }

    if let Some(path) = forbidden_path_argument(security, &job.command) {
        return JobRunOutcome {
            success: false,
            output: format!("blocked by security policy: forbidden path argument: {path}"),
            tenant_id: None,
            remove_after_run: false,
            run_id: None,
        };
    }

    let output = Command::new("sh")
        .arg("-lc")
        .arg(&job.command)
        .current_dir(&config.workspace_dir)
        .output()
        .await;

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!(
                "status={}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                stdout.trim(),
                stderr.trim()
            );
            JobRunOutcome {
                success: output.status.success(),
                output: combined,
                tenant_id: None,
                remove_after_run: false,
                run_id: None,
            }
        }
        Err(e) => JobRunOutcome {
            success: false,
            output: format!("spawn error: {e}"),
            tenant_id: None,
            remove_after_run: false,
            run_id: None,
        },
    }
}

fn normalize_feed_id(raw: &str) -> Option<String> {
    let trimmed = strip_wrapping_quotes(raw)
        .trim()
        .trim_end_matches(|c: char| c == ',' || c == ';');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn extract_feed_id_from_registry_execute_path(command: &str) -> Option<String> {
    for marker in ["/api/v1/registry/feeds/", "/registry/feeds/"] {
        if let Some(start) = command.find(marker) {
            let remainder = &command[start + marker.len()..];
            if let Some(end) = remainder.find("/execute") {
                return normalize_feed_id(&remainder[..end]);
            }
        }
    }
    None
}

fn parse_feed_execution_command(command: &str) -> Option<String> {
    let command = command.trim();

    if let Some(raw_id) = command.strip_prefix(INTERNAL_FEED_COMMAND_PREFIX) {
        return normalize_feed_id(raw_id);
    }

    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.len() >= 4
        && tokens[0] == "afw"
        && tokens[1] == "feed"
        && matches!(tokens[2], "run" | "execute")
    {
        return normalize_feed_id(tokens[3]);
    }

    if tokens.first() == Some(&"curl") {
        return extract_feed_id_from_registry_execute_path(command);
    }

    None
}

async fn run_internal_feed_execution(config: &Config, feed_id: &str) -> JobRunOutcome {
    let db_path = config.registry_db_path();
    let db = match aria::db::AriaDb::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            return JobRunOutcome {
                success: false,
                output: format!("failed to open aria db: {e}"),
                tenant_id: None,
                remove_after_run: false,
                run_id: None,
            }
        }
    };

    let scheduler = FeedScheduler::new(db.clone(), FeedExecutor::new(db));
    match scheduler.execute_feed(feed_id).await {
        Ok(()) => JobRunOutcome {
            success: true,
            output: format!("feed executed: {feed_id}"),
            tenant_id: None,
            remove_after_run: false,
            run_id: None,
        },
        Err(e) => JobRunOutcome {
            success: false,
            output: format!("feed execution failed: {e}"),
            tenant_id: None,
            remove_after_run: false,
            run_id: None,
        },
    }
}

fn payload_message(payload_kind: &str, payload_data: &str, name: &str) -> (String, Option<String>) {
    let parsed: Value = serde_json::from_str(payload_data).unwrap_or_else(|_| Value::Null);
    match payload_kind {
        "systemEvent" => {
            let text = parsed
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| payload_data.to_string());
            (format!("[Cron:{name}] {text}"), None)
        }
        "agentTurn" => {
            let text = parsed
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("cron trigger")
                .to_string();
            let model = parsed
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned);
            (text, model)
        }
        _ => (format!("[Cron:{name}] trigger"), None),
    }
}

fn format_heartbeat_summary(name: &str, message: &str) -> String {
    let trimmed = message.trim();
    if trimmed.starts_with("[Cron:") {
        trimmed.to_string()
    } else {
        format!("[Cron:{name}] {trimmed}")
    }
}

fn heartbeat_bullet_line(task: &str) -> String {
    format!("- {HEARTBEAT_QUEUE_PREFIX}{task}")
}

fn append_heartbeat_task(workspace_dir: &std::path::Path, task: &str) -> Result<()> {
    let heartbeat_path = workspace_dir.join("HEARTBEAT.md");
    if !heartbeat_path.exists() {
        std::fs::write(
            &heartbeat_path,
            "# Periodic Tasks\n\n# Add tasks below (one per line, starting with `- `)\n",
        )?;
    }
    let mut content = std::fs::read_to_string(&heartbeat_path).unwrap_or_default();
    let queued_line = heartbeat_bullet_line(task);
    if content.lines().any(|line| line.trim_end() == queued_line) {
        return Ok(());
    }
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&queued_line);
    content.push('\n');
    std::fs::write(&heartbeat_path, content)?;
    Ok(())
}

fn consume_delete_after_run(config: &Config, cron_func_id: &str) -> Result<()> {
    let db = aria::db::AriaDb::open(&config.registry_db_path())?;
    let now = Utc::now().to_rfc3339();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE aria_cron_functions
             SET enabled = 0, status = 'consumed', cron_job_id = NULL, updated_at = ?1
             WHERE id = ?2",
            params![now, cron_func_id],
        )?;
        Ok(())
    })
}

async fn run_aria_cron_function(config: &Config, cron_func_id: &str) -> JobRunOutcome {
    let db_path = config.registry_db_path();
    let db = match aria::db::AriaDb::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            return JobRunOutcome {
                success: false,
                output: format!("failed to open aria db: {e}"),
                tenant_id: None,
                remove_after_run: false,
                run_id: None,
            }
        }
    };

    let row = db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT tenant_id, name, payload_kind, payload_data, wake_mode, enabled, status, delete_after_run
             FROM aria_cron_functions WHERE id = ?1",
        )?;
        let found = stmt.query_row(params![cron_func_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)? != 0,
                r.get::<_, String>(6)?,
                r.get::<_, i64>(7)? != 0,
            ))
        });
        match found {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    });

    let Some((
        tenant_id,
        name,
        payload_kind,
        payload_data,
        wake_mode,
        enabled,
        status,
        delete_after_run,
    )) = row.unwrap_or(None)
    else {
        return JobRunOutcome {
            success: true,
            output: format!("cron function {cron_func_id} not found, removing orphaned job"),
            tenant_id: None,
            remove_after_run: true,
            run_id: None,
        };
    };

    if !enabled || status != "active" {
        return JobRunOutcome {
            success: true,
            output: format!("cron function {name} is disabled/inactive, removing job"),
            tenant_id: Some(tenant_id),
            remove_after_run: true,
            run_id: None,
        };
    }

    let (message, model) = payload_message(&payload_kind, &payload_data, &name);
    let run_id = Uuid::new_v4().to_string();

    if wake_mode == "next-heartbeat" {
        let summary = format_heartbeat_summary(&name, &message);
        match append_heartbeat_task(&config.workspace_dir, &summary) {
            Ok(()) => {
                if delete_after_run {
                    let _ = consume_delete_after_run(config, cron_func_id);
                }
                JobRunOutcome {
                    success: true,
                    output: "queued for next heartbeat".to_string(),
                    tenant_id: Some(tenant_id),
                    remove_after_run: delete_after_run,
                    run_id: Some(run_id),
                }
            }
            Err(e) => JobRunOutcome {
                success: false,
                output: format!("failed to queue heartbeat task: {e}"),
                tenant_id: Some(tenant_id),
                remove_after_run: false,
                run_id: Some(run_id),
            },
        }
    } else {
        match execute_agent_turn(config, &tenant_id, &message, model.as_deref()).await {
            Ok(output) => {
                if delete_after_run {
                    let _ = consume_delete_after_run(config, cron_func_id);
                }
                JobRunOutcome {
                    success: true,
                    output,
                    tenant_id: Some(tenant_id),
                    remove_after_run: delete_after_run,
                    run_id: Some(run_id),
                }
            }
            Err(e) => JobRunOutcome {
                success: false,
                output: format!("agent execution failed: {e}"),
                tenant_id: Some(tenant_id),
                remove_after_run: false,
                run_id: Some(run_id),
            },
        }
    }
}

async fn execute_agent_turn(
    config: &Config,
    tenant_id: &str,
    message: &str,
    model_override: Option<&str>,
) -> Result<String> {
    let provider_name = config.default_provider.as_deref().unwrap_or("openrouter");
    let model_name = model_override
        .or(config.default_model.as_deref())
        .unwrap_or("anthropic/claude-sonnet-4-20250514");

    let provider = providers::create_resilient_provider(
        provider_name,
        config.api_key.as_deref(),
        &config.reliability,
    )?;
    let mem = memory::create_memory(
        &config.memory,
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?;
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));
    let registry_db = aria::db::AriaDb::open(&config.registry_db_path())?;

    let result = agent::orchestrator::run_live_turn(
        agent::orchestrator::LiveTurnConfig {
            provider: provider.as_ref(),
            security: &security,
            memory: Arc::from(mem),
            composio_api_key: if config.composio.enabled {
                config.composio.api_key.as_deref()
            } else {
                None
            },
            browser_config: &config.browser,
            registry_db: &registry_db,
            workspace_dir: &config.workspace_dir,
            tenant_id,
            model: model_name,
            temperature: config.default_temperature,
            mode_hint: "cron",
            max_turns: Some(25),
            external_tool_context: None,
        },
        message,
        None,
    )
    .await?;

    Ok(result.output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aria::db::AriaDb;
    use crate::config::Config;
    use crate::security::SecurityPolicy;
    use chrono::Utc;
    use rusqlite::params;
    use tempfile::TempDir;

    fn test_config(tmp: &TempDir) -> Config {
        let mut config = Config::default();
        config.workspace_dir = tmp.path().join("workspace");
        config.config_path = tmp.path().join("config.toml");
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        config
    }

    fn test_job(command: &str) -> CronJob {
        CronJob {
            id: "test-job".into(),
            expression: "* * * * *".into(),
            timezone: None,
            command: command.into(),
            next_run: Utc::now(),
            last_run: None,
            last_status: None,
        }
    }

    fn insert_active_feed(config: &Config, feed_id: &str) {
        let db = AriaDb::open(&config.registry_db_path()).unwrap();
        let now = Utc::now().to_rfc3339();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO aria_feeds (id, tenant_id, name, schedule, handler_code, status, created_at, updated_at)
                 VALUES (?1, 'test-tenant', ?2, '*/5 * * * *', 'console.log(\"ok\")', 'active', ?3, ?3)",
                params![feed_id, format!("feed-{feed_id}"), now],
            )?;
            Ok(())
        })
        .unwrap();
    }

    #[tokio::test]
    async fn run_job_command_success() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = test_job("echo scheduler-ok");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let outcome = run_job_command(&config, &security, &job).await;
        assert!(outcome.success);
        assert!(outcome.output.contains("scheduler-ok"));
        assert!(outcome.output.contains("status=exit status: 0"));
    }

    #[tokio::test]
    async fn run_job_command_failure() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = test_job("ls definitely_missing_file_for_scheduler_test");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let outcome = run_job_command(&config, &security, &job).await;
        assert!(!outcome.success);
        assert!(outcome
            .output
            .contains("definitely_missing_file_for_scheduler_test"));
        assert!(outcome.output.contains("status=exit status:"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_disallowed_command() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.autonomy.allowed_commands = vec!["echo".into()];
        let job = test_job("curl https://evil.example");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let outcome = run_job_command(&config, &security, &job).await;
        assert!(!outcome.success);
        assert!(outcome.output.contains("blocked by security policy"));
        assert!(outcome.output.contains("command not allowed"));
    }

    #[tokio::test]
    async fn run_job_command_blocks_forbidden_path_argument() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.autonomy.allowed_commands = vec!["cat".into()];
        let job = test_job("cat /etc/passwd");
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let outcome = run_job_command(&config, &security, &job).await;
        assert!(!outcome.success);
        assert!(outcome.output.contains("blocked by security policy"));
        assert!(outcome.output.contains("forbidden path argument"));
        assert!(outcome.output.contains("/etc/passwd"));
    }

    #[test]
    fn parse_feed_execution_command_supports_expected_shapes() {
        assert_eq!(
            parse_feed_execution_command("__ARIA_FEED_EXEC__:abc123").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            parse_feed_execution_command("afw feed run abc123").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            parse_feed_execution_command("afw feed execute abc123").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            parse_feed_execution_command(
                "curl -X POST http://127.0.0.1:8080/api/v1/registry/feeds/abc123/execute",
            )
            .as_deref(),
            Some("abc123")
        );
        assert_eq!(parse_feed_execution_command("echo hello"), None);
    }

    #[tokio::test]
    async fn run_job_command_executes_feed_without_shell_policy_allowlist() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.autonomy.allowed_commands = vec!["echo".into()];
        insert_active_feed(&config, "feed-cron-test");

        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);
        let job = test_job("afw feed run feed-cron-test");

        let outcome = run_job_command(&config, &security, &job).await;
        assert!(outcome.success);
        assert_eq!(outcome.output, "feed executed: feed-cron-test");
    }

    #[tokio::test]
    async fn execute_job_with_retry_recovers_after_first_failure() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.reliability.scheduler_retries = 1;
        config.reliability.provider_backoff_ms = 1;
        config.autonomy.allowed_commands = vec!["sh".into()];
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        std::fs::write(
            config.workspace_dir.join("retry-once.sh"),
            "#!/bin/sh\nif [ -f retry-ok.flag ]; then\n  echo recovered\n  exit 0\nfi\ntouch retry-ok.flag\nexit 1\n",
        )
        .unwrap();
        let job = test_job("sh ./retry-once.sh");

        let outcome = execute_job_with_retry(&config, &security, &job).await;
        assert!(outcome.success);
        assert!(outcome.output.contains("recovered"));
    }

    #[tokio::test]
    async fn execute_job_with_retry_exhausts_attempts() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.reliability.scheduler_retries = 1;
        config.reliability.provider_backoff_ms = 1;
        let security = SecurityPolicy::from_config(&config.autonomy, &config.workspace_dir);

        let job = test_job("ls always_missing_for_retry_test");

        let outcome = execute_job_with_retry(&config, &security, &job).await;
        assert!(!outcome.success);
        assert!(outcome.output.contains("always_missing_for_retry_test"));
    }

    #[test]
    fn format_heartbeat_summary_avoids_double_cron_prefix() {
        let prefixed = format_heartbeat_summary("daily-job", "[Cron:daily-job] hello");
        assert_eq!(prefixed, "[Cron:daily-job] hello");

        let plain = format_heartbeat_summary("daily-job", "hello");
        assert_eq!(plain, "[Cron:daily-job] hello");
    }

    #[test]
    fn append_heartbeat_task_deduplicates_queue_line() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path();
        append_heartbeat_task(workspace, "[Cron:test] once").unwrap();
        append_heartbeat_task(workspace, "[Cron:test] once").unwrap();

        let content = std::fs::read_to_string(workspace.join("HEARTBEAT.md")).unwrap();
        let count = content
            .lines()
            .filter(|line| line.contains("[[AFW_QUEUE]] [Cron:test] once"))
            .count();
        assert_eq!(count, 1);
    }
}
