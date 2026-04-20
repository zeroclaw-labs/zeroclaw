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

#[test]
fn backup_cron_job_synced_when_schedule_set() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp, Some("0 3 * * *".to_string()));

    // Synthesize builtin backup job from config.backup.schedule_cron
    let mut jobs_with_builtin = config.cron.jobs.clone();
    if let Some(schedule_cron) = &config.backup.schedule_cron {
        let backup_job = CronJobDecl {
            id: "__builtin_backup".to_string(),
            name: Some("Scheduled backup".to_string()),
            job_type: "shell".to_string(),
            schedule: CronScheduleDecl::Cron {
                expr: schedule_cron.clone(),
                tz: None,
            },
            command: Some("backup create".to_string()),
            prompt: None,
            enabled: true,
            model: None,
            allowed_tools: None,
            session_target: None,
            uses_memory: true,
            delivery: None,
        };
        jobs_with_builtin.push(backup_job);
    }

    sync_declarative_jobs(&config, &jobs_with_builtin).unwrap();

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

    // No builtin backup job should be synthesized
    let jobs_with_builtin = config.cron.jobs.clone();
    sync_declarative_jobs(&config, &jobs_with_builtin).unwrap();

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

    // First sync: create the builtin backup job
    let mut jobs_with_builtin = config_with_schedule.cron.jobs.clone();
    if let Some(schedule_cron) = &config_with_schedule.backup.schedule_cron {
        let backup_job = CronJobDecl {
            id: "__builtin_backup".to_string(),
            name: Some("Scheduled backup".to_string()),
            job_type: "shell".to_string(),
            schedule: CronScheduleDecl::Cron {
                expr: schedule_cron.clone(),
                tz: None,
            },
            command: Some("backup create".to_string()),
            prompt: None,
            enabled: true,
            model: None,
            allowed_tools: None,
            session_target: None,
            uses_memory: true,
            delivery: None,
        };
        jobs_with_builtin.push(backup_job);
    }
    sync_declarative_jobs(&config_with_schedule, &jobs_with_builtin).unwrap();
    assert!(get_job(&config_with_schedule, "__builtin_backup").is_ok());

    // Second sync: remove schedule_cron from config
    let config_without_schedule = test_config(&tmp, None);
    let jobs_no_builtin = config_without_schedule.cron.jobs.clone();
    sync_declarative_jobs(&config_without_schedule, &jobs_no_builtin).unwrap();

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

    // First sync with schedule "0 3 * * *"
    let mut jobs_v1 = config_v1.cron.jobs.clone();
    if let Some(schedule_cron) = &config_v1.backup.schedule_cron {
        let backup_job = CronJobDecl {
            id: "__builtin_backup".to_string(),
            name: Some("Scheduled backup".to_string()),
            job_type: "shell".to_string(),
            schedule: CronScheduleDecl::Cron {
                expr: schedule_cron.clone(),
                tz: None,
            },
            command: Some("backup create".to_string()),
            prompt: None,
            enabled: true,
            model: None,
            allowed_tools: None,
            session_target: None,
            uses_memory: true,
            delivery: None,
        };
        jobs_v1.push(backup_job);
    }
    sync_declarative_jobs(&config_v1, &jobs_v1).unwrap();

    let job_v1 = get_job(&config_v1, "__builtin_backup").unwrap();
    let next_run_v1 = job_v1.next_run;

    // Second sync with schedule "0 2 * * *"
    let config_v2 = test_config(&tmp, Some("0 2 * * *".to_string()));
    let mut jobs_v2 = config_v2.cron.jobs.clone();
    if let Some(schedule_cron) = &config_v2.backup.schedule_cron {
        let backup_job = CronJobDecl {
            id: "__builtin_backup".to_string(),
            name: Some("Scheduled backup".to_string()),
            job_type: "shell".to_string(),
            schedule: CronScheduleDecl::Cron {
                expr: schedule_cron.clone(),
                tz: None,
            },
            command: Some("backup create".to_string()),
            prompt: None,
            enabled: true,
            model: None,
            allowed_tools: None,
            session_target: None,
            uses_memory: true,
            delivery: None,
        };
        jobs_v2.push(backup_job);
    }
    sync_declarative_jobs(&config_v2, &jobs_v2).unwrap();

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

    // Sync twice with same config
    for _ in 0..2 {
        let mut jobs_with_builtin = config.cron.jobs.clone();
        if let Some(schedule_cron) = &config.backup.schedule_cron {
            let backup_job = CronJobDecl {
                id: "__builtin_backup".to_string(),
                name: Some("Scheduled backup".to_string()),
                job_type: "shell".to_string(),
                schedule: CronScheduleDecl::Cron {
                    expr: schedule_cron.clone(),
                    tz: None,
                },
                command: Some("backup create".to_string()),
                prompt: None,
                enabled: true,
                model: None,
                allowed_tools: None,
                session_target: None,
                uses_memory: true,
                delivery: None,
            };
            jobs_with_builtin.push(backup_job);
        }
        sync_declarative_jobs(&config, &jobs_with_builtin).unwrap();
    }

    // Verify only one job exists with stable ID
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

    let mut jobs_with_builtin = config.cron.jobs.clone();
    if let Some(schedule_cron) = &config.backup.schedule_cron {
        let backup_job = CronJobDecl {
            id: "__builtin_backup".to_string(),
            name: Some("Scheduled backup".to_string()),
            job_type: "shell".to_string(),
            schedule: CronScheduleDecl::Cron {
                expr: schedule_cron.clone(),
                tz: None,
            },
            command: Some("backup create".to_string()),
            prompt: None,
            enabled: true,
            model: None,
            allowed_tools: None,
            session_target: None,
            uses_memory: true,
            delivery: None,
        };
        jobs_with_builtin.push(backup_job);
    }
    sync_declarative_jobs(&config, &jobs_with_builtin).unwrap();

    let job = get_job(&config, "__builtin_backup").unwrap();
    assert_eq!(job.command, "backup create");
}

#[test]
fn backup_cron_job_type_is_shell() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp, Some("0 3 * * *".to_string()));

    let mut jobs_with_builtin = config.cron.jobs.clone();
    if let Some(schedule_cron) = &config.backup.schedule_cron {
        let backup_job = CronJobDecl {
            id: "__builtin_backup".to_string(),
            name: Some("Scheduled backup".to_string()),
            job_type: "shell".to_string(),
            schedule: CronScheduleDecl::Cron {
                expr: schedule_cron.clone(),
                tz: None,
            },
            command: Some("backup create".to_string()),
            prompt: None,
            enabled: true,
            model: None,
            allowed_tools: None,
            session_target: None,
            uses_memory: true,
            delivery: None,
        };
        jobs_with_builtin.push(backup_job);
    }
    sync_declarative_jobs(&config, &jobs_with_builtin).unwrap();

    let job = get_job(&config, "__builtin_backup").unwrap();
    assert_eq!(job.job_type, JobType::Shell);
}

#[test]
fn backup_cron_job_source_is_declarative() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp, Some("0 3 * * *".to_string()));

    let mut jobs_with_builtin = config.cron.jobs.clone();
    if let Some(schedule_cron) = &config.backup.schedule_cron {
        let backup_job = CronJobDecl {
            id: "__builtin_backup".to_string(),
            name: Some("Scheduled backup".to_string()),
            job_type: "shell".to_string(),
            schedule: CronScheduleDecl::Cron {
                expr: schedule_cron.clone(),
                tz: None,
            },
            command: Some("backup create".to_string()),
            prompt: None,
            enabled: true,
            model: None,
            allowed_tools: None,
            session_target: None,
            uses_memory: true,
            delivery: None,
        };
        jobs_with_builtin.push(backup_job);
    }
    sync_declarative_jobs(&config, &jobs_with_builtin).unwrap();

    let job = get_job(&config, "__builtin_backup").unwrap();
    assert_eq!(job.source, "declarative");
}
