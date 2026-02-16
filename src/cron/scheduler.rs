use crate::config::Config;
use crate::cron::{due_jobs, reschedule_after_run, CronJob};
use crate::security::SecurityPolicy;
use crate::status_events;
use crate::{agent, aria};
use anyhow::Result;
use chrono::Utc;
use rusqlite::params;
use serde_json::Value;
use tokio::process::Command;
use tokio::time::{self, Duration};

const MIN_POLL_SECONDS: u64 = 5;

#[derive(Debug, Clone)]
struct JobRunOutcome {
    success: bool,
    output: String,
    tenant_id: Option<String>,
    remove_after_run: bool,
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
            let tenant = outcome.tenant_id.clone();
            if outcome.success {
                status_events::emit(
                    "cron.completed",
                    serde_json::json!({
                        "jobId": job.id,
                        "summary": outcome.output.lines().next().unwrap_or("ok"),
                        "tenantId": tenant,
                    }),
                );
            } else {
                status_events::emit(
                    "cron.failed",
                    serde_json::json!({
                        "jobId": job.id,
                        "error": outcome.output,
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

    if !security.is_command_allowed(&job.command) {
        return JobRunOutcome {
            success: false,
            output: format!(
                "blocked by security policy: command not allowed: {}",
                job.command
            ),
            tenant_id: None,
            remove_after_run: false,
        };
    }

    if let Some(path) = forbidden_path_argument(security, &job.command) {
        return JobRunOutcome {
            success: false,
            output: format!("blocked by security policy: forbidden path argument: {path}"),
            tenant_id: None,
            remove_after_run: false,
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
            }
        }
        Err(e) => JobRunOutcome {
            success: false,
            output: format!("spawn error: {e}"),
            tenant_id: None,
            remove_after_run: false,
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

fn append_heartbeat_task(workspace_dir: &std::path::Path, task: &str) -> Result<()> {
    let heartbeat_path = workspace_dir.join("HEARTBEAT.md");
    if !heartbeat_path.exists() {
        std::fs::write(
            &heartbeat_path,
            "# Periodic Tasks\n\n# Add tasks below (one per line, starting with `- `)\n",
        )?;
    }
    let mut content = std::fs::read_to_string(&heartbeat_path).unwrap_or_default();
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str("- ");
    content.push_str(task);
    content.push('\n');
    std::fs::write(&heartbeat_path, content)?;
    Ok(())
}

fn consume_delete_after_run(config: &Config, cron_func_id: &str) -> Result<()> {
    let db = aria::db::AriaDb::open(&config.workspace_dir.join("aria.db"))?;
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
    let db_path = config.workspace_dir.join("aria.db");
    let db = match aria::db::AriaDb::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            return JobRunOutcome {
                success: false,
                output: format!("failed to open aria db: {e}"),
                tenant_id: None,
                remove_after_run: false,
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
        };
    };

    if !enabled || status != "active" {
        return JobRunOutcome {
            success: true,
            output: format!("cron function {name} is disabled/inactive, removing job"),
            tenant_id: Some(tenant_id),
            remove_after_run: true,
        };
    }

    let (message, model) = payload_message(&payload_kind, &payload_data, &name);

    if wake_mode == "next-heartbeat" {
        let summary = format!("[Cron:{name}] {message}");
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
                }
            }
            Err(e) => JobRunOutcome {
                success: false,
                output: format!("failed to queue heartbeat task: {e}"),
                tenant_id: Some(tenant_id),
                remove_after_run: false,
            },
        }
    } else {
        let temp = config.default_temperature;
        match agent::run(config.clone(), Some(message), None, model, temp).await {
            Ok(_) => {
                if delete_after_run {
                    let _ = consume_delete_after_run(config, cron_func_id);
                }
                JobRunOutcome {
                    success: true,
                    output: "ran immediately".to_string(),
                    tenant_id: Some(tenant_id),
                    remove_after_run: delete_after_run,
                }
            }
            Err(e) => JobRunOutcome {
                success: false,
                output: format!("agent execution failed: {e}"),
                tenant_id: Some(tenant_id),
                remove_after_run: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::security::SecurityPolicy;
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
}
