use std::collections::HashMap;
use tempfile::TempDir;
use zeroclaw::config::Config;
use zeroclaw::config::schema::{CronJobDecl, CronScheduleDecl};
use zeroclaw::cron::{JobType, Schedule, get_job, list_jobs, sync_declarative_jobs};

fn test_config(tmp: &TempDir, schedule_cron: Option<String>) -> Config {
    let mut config = Config {
        workspace_dir: tmp.path().join("workspace"),
        config_path: tmp.path().join("config.toml"),
        ..Config::default()
    };
    config.backup.schedule_cron = schedule_cron;
    std::fs::create_dir_all(&config.workspace_dir).unwrap();
    config
}

fn jobs_with_backup(config: &Config) -> HashMap<String, CronJobDecl> {
    let mut jobs = config.cron.clone();
    if let Some(schedule_cron) = &config.backup.schedule_cron {
        jobs.insert(
            "__builtin_backup".to_string(),
            CronJobDecl {
                name: Some("Scheduled backup".to_string()),
                job_type: "shell".to_string(),
                schedule: CronScheduleDecl::Cron {
                    expr: schedule_cron.clone(),
                    tz: None,
                },
                command: Some("backup create".to_string()),
                ..Default::default()
            },
        );
    }
    jobs
}

#[test]
fn backup_cron_job_synced_when_schedule_set() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp, Some("0 3 * * *".to_string()));

    sync_declarative_jobs(&config, &jobs_with_backup(&config)).unwrap();

    let job = get_job(&config, "__builtin_backup").unwrap();
    assert_eq!(job.id, "__builtin_backup");
    assert_eq!(job.command, "backup create");
    assert_eq!(job.source, "declarative");
    assert!(matches!(job.schedule, Schedule::Cron { ref expr, .. } if expr == "0 3 * * *"));
}

#[test]
fn backup_cron_job_not_synced_when_schedule_none() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp, None);

    sync_declarative_jobs(&config, &jobs_with_backup(&config)).unwrap();

    let result = get_job(&config, "__builtin_backup");
    assert!(
        result.is_err(),
        "builtin backup job should not exist when schedule_cron is None"
    );
}

#[test]
fn backup_cron_job_removed_when_schedule_cleared() {
    let tmp = TempDir::new().unwrap();
    let config_with_schedule = test_config(&tmp, Some("0 3 * * *".to_string()));

    sync_declarative_jobs(
        &config_with_schedule,
        &jobs_with_backup(&config_with_schedule),
    )
    .unwrap();
    assert!(get_job(&config_with_schedule, "__builtin_backup").is_ok());

    let config_without_schedule = test_config(&tmp, None);
    sync_declarative_jobs(
        &config_without_schedule,
        &jobs_with_backup(&config_without_schedule),
    )
    .unwrap();

    let result = get_job(&config_without_schedule, "__builtin_backup");
    assert!(
        result.is_err(),
        "builtin backup job should be removed when schedule_cron is cleared"
    );
}

#[test]
fn backup_cron_job_schedule_updated() {
    let tmp = TempDir::new().unwrap();
    let config_v1 = test_config(&tmp, Some("0 3 * * *".to_string()));

    sync_declarative_jobs(&config_v1, &jobs_with_backup(&config_v1)).unwrap();

    let job_v1 = get_job(&config_v1, "__builtin_backup").unwrap();
    let next_run_v1 = job_v1.next_run;

    let config_v2 = test_config(&tmp, Some("0 2 * * *".to_string()));
    sync_declarative_jobs(&config_v2, &jobs_with_backup(&config_v2)).unwrap();

    let job_v2 = get_job(&config_v2, "__builtin_backup").unwrap();
    assert!(matches!(job_v2.schedule, Schedule::Cron { ref expr, .. } if expr == "0 2 * * *"));
    assert_ne!(
        job_v2.next_run, next_run_v1,
        "next_run should be recalculated when schedule changes"
    );
}

#[test]
fn backup_cron_job_id_is_stable() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp, Some("0 3 * * *".to_string()));

    for _ in 0..2 {
        sync_declarative_jobs(&config, &jobs_with_backup(&config)).unwrap();
    }

    let job = get_job(&config, "__builtin_backup").unwrap();
    assert_eq!(job.id, "__builtin_backup");

    let all_jobs = list_jobs(&config).unwrap();
    let backup_jobs: Vec<_> = all_jobs
        .iter()
        .filter(|j| j.id == "__builtin_backup")
        .collect();
    assert_eq!(
        backup_jobs.len(),
        1,
        "should have exactly one builtin backup job, not duplicates"
    );
}

#[test]
fn backup_cron_job_command_is_backup_create() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp, Some("0 3 * * *".to_string()));

    sync_declarative_jobs(&config, &jobs_with_backup(&config)).unwrap();

    let job = get_job(&config, "__builtin_backup").unwrap();
    assert_eq!(job.command, "backup create");
}

#[test]
fn backup_cron_job_type_is_shell() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp, Some("0 3 * * *".to_string()));

    sync_declarative_jobs(&config, &jobs_with_backup(&config)).unwrap();

    let job = get_job(&config, "__builtin_backup").unwrap();
    assert_eq!(job.job_type, JobType::Shell);
}

#[test]
fn backup_cron_job_source_is_declarative() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp, Some("0 3 * * *".to_string()));

    sync_declarative_jobs(&config, &jobs_with_backup(&config)).unwrap();

    let job = get_job(&config, "__builtin_backup").unwrap();
    assert_eq!(job.source, "declarative");
}
