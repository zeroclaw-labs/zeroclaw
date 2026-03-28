use crate::config::Config;
use crate::cron::{add_agent_job, CronJob, Schedule, SessionTarget};
use anyhow::Result;

/// Default cron expression: 3:00 AM daily.
const DEFAULT_SCHEDULE_EXPR: &str = "0 3 * * *";

/// Job name marker used to identify consolidation jobs.
pub const CONSOLIDATION_JOB_NAME: &str = "__consolidate_nightly";

/// The prompt instructs the agent to perform memory consolidation using
/// existing tools (cron_runs, memory_recall, memory_store, file_write).
const CONSOLIDATION_PROMPT: &str = "\
You are running a nightly memory consolidation job. Your goal is to distill \
the past 24 hours of operational activity into a concise, actionable summary \
stored in long-term memory.

Follow these steps exactly:

1. Use `cron_runs` to review recent job execution results from the past 24 hours. \
   Note any recurring errors, timeouts, or policy denials.

2. Use `memory_recall` to retrieve today's Daily memories. Look for patterns, \
   discoveries, and progress toward goals.

3. Identify and classify findings:
   - **Recurring errors**: problems that appeared more than once
   - **Successful strategies**: approaches that worked well
   - **New discoveries**: information or capabilities learned
   - **Blocked goals**: objectives that could not be completed and why

4. Synthesize a concise summary (max 500 words) of actionable learnings. \
   Focus on what should change going forward, not just what happened.

5. Store the summary using `memory_store` with category \"core\" and \
   key format \"consolidation_YYYY-MM-DD\" (use today's date).

6. If the workspace file `MEMORY.md` exists, use `file_read` to read it, \
   then use `file_write` to append a dated section at the end with the \
   top 3 learnings from today's consolidation. Format:
   ```
   ## Consolidation — YYYY-MM-DD
   1. <learning 1>
   2. <learning 2>
   3. <learning 3>
   ```

If there is no meaningful activity to consolidate (no runs, no daily memories), \
store a brief note confirming the check was performed and skip the MEMORY.md update.";

/// Create a nightly memory consolidation cron agent job.
///
/// Schedule: 3:00 AM daily (local time), configurable via `schedule_expr`.
/// Job type: agent with `__consolidate` marker in the name.
/// Session target: isolated (does not disturb main sessions).
pub fn create_consolidation_job(config: &Config) -> Result<CronJob> {
    create_consolidation_job_with_schedule(config, DEFAULT_SCHEDULE_EXPR, None)
}

/// Create a consolidation job with a custom cron expression and optional timezone.
pub fn create_consolidation_job_with_schedule(
    config: &Config,
    cron_expr: &str,
    tz: Option<String>,
) -> Result<CronJob> {
    let schedule = Schedule::Cron {
        expr: cron_expr.into(),
        tz,
    };

    add_agent_job(
        config,
        Some(CONSOLIDATION_JOB_NAME.into()),
        schedule,
        CONSOLIDATION_PROMPT,
        SessionTarget::Isolated,
        None,  // use default model
        None,  // no delivery config
        false, // recurring job — do not delete after run
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron::{JobType, Schedule, SessionTarget};
    use tempfile::TempDir;

    fn test_config(tmp: &TempDir) -> Config {
        let config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        config
    }

    #[test]
    fn create_consolidation_job_produces_valid_job() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = create_consolidation_job(&config).unwrap();

        assert_eq!(job.name.as_deref(), Some(CONSOLIDATION_JOB_NAME));
        assert_eq!(job.job_type, JobType::Agent);
        assert_eq!(job.session_target, SessionTarget::Isolated);
        assert!(!job.delete_after_run);
        assert!(job.enabled);
    }

    #[test]
    fn create_consolidation_job_uses_correct_schedule() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = create_consolidation_job(&config).unwrap();

        match &job.schedule {
            Schedule::Cron { expr, tz } => {
                assert_eq!(expr, DEFAULT_SCHEDULE_EXPR);
                assert!(tz.is_none());
            }
            other => panic!("Expected Cron schedule, got {other:?}"),
        }
    }

    #[test]
    fn create_consolidation_job_prompt_contains_key_instructions() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = create_consolidation_job(&config).unwrap();
        let prompt = job.prompt.expect("consolidation job must have a prompt");

        assert!(
            prompt.contains("memory_recall"),
            "prompt should instruct use of memory_recall"
        );
        assert!(
            prompt.contains("memory_store"),
            "prompt should instruct use of memory_store"
        );
        assert!(
            prompt.contains("cron_runs"),
            "prompt should instruct use of cron_runs"
        );
        assert!(
            prompt.contains("consolidation_YYYY-MM-DD"),
            "prompt should specify key format"
        );
        assert!(
            prompt.contains("core"),
            "prompt should specify core category"
        );
        assert!(
            prompt.contains("MEMORY.md"),
            "prompt should mention MEMORY.md"
        );
    }

    #[test]
    fn create_consolidation_job_with_custom_schedule_applies_tz() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let job = create_consolidation_job_with_schedule(
            &config,
            "0 4 * * *",
            Some("America/New_York".into()),
        )
        .unwrap();

        match &job.schedule {
            Schedule::Cron { expr, tz } => {
                assert_eq!(expr, "0 4 * * *");
                assert_eq!(tz.as_deref(), Some("America/New_York"));
            }
            other => panic!("Expected Cron schedule, got {other:?}"),
        }
    }
}
