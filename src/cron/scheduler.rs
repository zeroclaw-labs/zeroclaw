use crate::config::Config;
use crate::cron::{due_jobs, reschedule_after_run, CronJob};
use anyhow::Result;
use chrono::Utc;
use tokio::process::Command;
use tokio::time::{self, Duration};

const MIN_POLL_SECONDS: u64 = 5;

pub async fn run(config: Config) -> Result<()> {
    let poll_secs = config.reliability.scheduler_poll_secs.max(MIN_POLL_SECONDS);
    let mut interval = time::interval(Duration::from_secs(poll_secs));

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
            let (success, output) = execute_job_with_retry(&config, &job).await;

            if !success {
                crate::health::mark_component_error("scheduler", format!("job {} failed", job.id));
            }

            if let Err(e) = reschedule_after_run(&config, &job, success, &output) {
                crate::health::mark_component_error("scheduler", e.to_string());
                tracing::warn!("Failed to persist scheduler run result: {e}");
            }
        }
    }
}

async fn execute_job_with_retry(config: &Config, job: &CronJob) -> (bool, String) {
    let mut last_output = String::new();
    let retries = config.reliability.scheduler_retries;
    let mut backoff_ms = config.reliability.provider_backoff_ms.max(200);

    for attempt in 0..=retries {
        let (success, output) = run_job_command(config, job).await;
        last_output = output;

        if success {
            return (true, last_output);
        }

        if attempt < retries {
            let jitter_ms = (Utc::now().timestamp_subsec_millis() % 250) as u64;
            time::sleep(Duration::from_millis(backoff_ms + jitter_ms)).await;
            backoff_ms = (backoff_ms.saturating_mul(2)).min(30_000);
        }
    }

    (false, last_output)
}

async fn run_job_command(config: &Config, job: &CronJob) -> (bool, String) {
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
            (output.status.success(), combined)
        }
        Err(e) => (false, format!("spawn error: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
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

        let (success, output) = run_job_command(&config, &job).await;
        assert!(success);
        assert!(output.contains("scheduler-ok"));
        assert!(output.contains("status=exit status: 0"));
    }

    #[tokio::test]
    async fn run_job_command_failure() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);
        let job = test_job("echo scheduler-fail 1>&2; exit 7");

        let (success, output) = run_job_command(&config, &job).await;
        assert!(!success);
        assert!(output.contains("scheduler-fail"));
        assert!(output.contains("status=exit status: 7"));
    }

    #[tokio::test]
    async fn execute_job_with_retry_recovers_after_first_failure() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.reliability.scheduler_retries = 1;
        config.reliability.provider_backoff_ms = 1;

        let job = test_job(
            "if [ -f retry-ok.flag ]; then echo recovered; exit 0; else touch retry-ok.flag; echo first-fail 1>&2; exit 1; fi",
        );

        let (success, output) = execute_job_with_retry(&config, &job).await;
        assert!(success);
        assert!(output.contains("recovered"));
    }

    #[tokio::test]
    async fn execute_job_with_retry_exhausts_attempts() {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp);
        config.reliability.scheduler_retries = 1;
        config.reliability.provider_backoff_ms = 1;

        let job = test_job("echo still-bad 1>&2; exit 1");

        let (success, output) = execute_job_with_retry(&config, &job).await;
        assert!(!success);
        assert!(output.contains("still-bad"));
    }
}
