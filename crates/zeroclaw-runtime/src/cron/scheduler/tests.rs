#[cfg(test)]
use super::*;
use crate::cron::{self, DeliveryConfig};
use crate::security::SecurityPolicy;
use chrono::{Duration as ChronoDuration, Utc};
use tempfile::TempDir;
use zeroclaw_config::schema::Config;

const TEST_AGENT: &str = "test-agent";

mod lifecycle;

#[test]
fn is_no_reply_sentinel_matches_bare_form_case_insensitively() {
    assert!(is_no_reply_sentinel("NO_REPLY"));
    assert!(is_no_reply_sentinel("no_reply"));
    assert!(is_no_reply_sentinel("No_Reply"));
    // Trim tolerance.
    assert!(is_no_reply_sentinel("  NO_REPLY  "));
    assert!(is_no_reply_sentinel("\nNO_REPLY\n"));
}

#[test]
fn is_no_reply_sentinel_matches_quiet_info_and_legacy_prefixes() {
    // Legacy form is documented as "treated as INFO".
    assert!(is_no_reply_sentinel("NO_REPLY: nothing to report"));
    assert!(is_no_reply_sentinel("  NO_REPLY: trimmed  "));
    // Explicit informational kind.
    assert!(is_no_reply_sentinel("NO_REPLY[INFO]: all healthy"));
    assert!(is_no_reply_sentinel("no_reply[info]: all healthy"));
    // Bracket whitespace tolerance.
    assert!(is_no_reply_sentinel("NO_REPLY[ info ]: spaced"));
}

#[test]
fn is_no_reply_sentinel_does_not_suppress_failure_or_refusal_kinds() {
    // REFUSE / FAIL carry operator-visible meaning. In the cron/heartbeat
    // announce context there is no reaction side-channel, so suppressing
    // them would silently drop a failure/refusal the operator must see
    // review feedback).
    assert!(!is_no_reply_sentinel(
        "NO_REPLY[FAIL]: database check timed out"
    ));
    assert!(!is_no_reply_sentinel("no_reply[fail]: timed out"));
    assert!(!is_no_reply_sentinel(
        "NO_REPLY[REFUSE]: policy prevented the check"
    ));
    assert!(!is_no_reply_sentinel("no_reply[refuse]: blocked"));
    // Unknown/future kinds are conservatively delivered, not suppressed.
    assert!(!is_no_reply_sentinel("NO_REPLY[WARN]: disk at 90%"));
    // Malformed kinded form with no closing bracket is delivered.
    assert!(!is_no_reply_sentinel("NO_REPLY[INFO without close"));
}

#[test]
fn is_no_reply_sentinel_rejects_real_content() {
    assert!(!is_no_reply_sentinel(""));
    assert!(!is_no_reply_sentinel("   "));
    assert!(!is_no_reply_sentinel("All systems nominal"));
    // Sentinel-looking but not a sentinel: word embedded in real prose.
    assert!(!is_no_reply_sentinel(
        "The job returned NO_REPLY which means nothing happened"
    ));
    assert!(!is_no_reply_sentinel("NO_REPLYING is the status"));
}

async fn test_config(tmp: &TempDir) -> Config {
    let mut config = Config {
        data_dir: tmp.path().join("data"),
        config_path: tmp.path().join("config.toml"),
        ..Config::default()
    };
    if !zeroclaw_memory::TACHI_COMPILED {
        config.memory.backend = "markdown".into();
    }
    config.risk_profiles.insert(
        TEST_AGENT.to_string(),
        zeroclaw_config::schema::RiskProfileConfig::default(),
    );
    config.runtime_profiles.insert(
        TEST_AGENT.to_string(),
        zeroclaw_config::schema::RuntimeProfileConfig::default(),
    );
    config.providers.models.openrouter.insert(
        TEST_AGENT.to_string(),
        zeroclaw_config::schema::OpenRouterModelProviderConfig::default(),
    );
    config.agents.insert(
        TEST_AGENT.to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            model_provider: format!("openrouter.{TEST_AGENT}").into(),
            risk_profile: TEST_AGENT.into(),
            runtime_profile: TEST_AGENT.into(),
            ..Default::default()
        },
    );
    tokio::fs::create_dir_all(&config.data_dir).await.unwrap();
    config
}

fn test_security(config: &Config) -> SecurityPolicy {
    SecurityPolicy::for_agent(config, TEST_AGENT).expect("test-agent has resolvable profiles")
}

fn test_job(command: &str) -> CronJob {
    CronJob {
        id: "test-job".into(),
        expression: "* * * * *".into(),
        schedule: crate::cron::Schedule::Cron {
            expr: "* * * * *".into(),
            tz: None,
        },
        command: command.into(),
        prompt: None,
        name: None,
        job_type: JobType::Shell,
        session_target: SessionTarget::Isolated,
        model: None,
        agent_alias: TEST_AGENT.into(),
        enabled: true,
        delivery: DeliveryConfig::default(),
        delete_after_run: false,
        allowed_tools: None,
        uses_memory: true,
        source: "imperative".into(),
        created_at: Utc::now(),
        next_run: Utc::now(),
        last_run: None,
        last_status: None,
        last_output: None,
    }
}

fn unique_component(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

fn agent_job_with_schedule(schedule: crate::cron::Schedule) -> CronJob {
    CronJob {
        job_type: JobType::Agent,
        schedule,
        ..test_job("echo test")
    }
}

#[test]
fn high_frequency_daily_cron_is_not_flagged() {
    // `0 6 * * *` fires once per day — must never warn regardless of when the check runs
    let job = agent_job_with_schedule(crate::cron::Schedule::Cron {
        expr: "0 6 * * *".into(),
        tz: Some("America/Chicago".into()),
    });
    assert!(!is_high_frequency_agent_job(&job));
}

#[test]
fn high_frequency_every_4min_cron_is_flagged() {
    let job = agent_job_with_schedule(crate::cron::Schedule::Cron {
        expr: "*/4 * * * *".into(),
        tz: None,
    });
    assert!(is_high_frequency_agent_job(&job));
}

#[test]
fn high_frequency_every_5min_cron_is_not_flagged() {
    // Exactly 5 minutes is acceptable (threshold is strictly less than 5)
    let job = agent_job_with_schedule(crate::cron::Schedule::Cron {
        expr: "*/5 * * * *".into(),
        tz: None,
    });
    assert!(!is_high_frequency_agent_job(&job));
}

#[test]
fn high_frequency_every_interval_below_threshold_is_flagged() {
    let job = agent_job_with_schedule(crate::cron::Schedule::Every {
        every_ms: 4 * 60 * 1000, // 4 minutes
    });
    assert!(is_high_frequency_agent_job(&job));
}

#[test]
fn high_frequency_every_interval_at_threshold_is_not_flagged() {
    let job = agent_job_with_schedule(crate::cron::Schedule::Every {
        every_ms: 5 * 60 * 1000, // exactly 5 minutes
    });
    assert!(!is_high_frequency_agent_job(&job));
}

#[test]
fn high_frequency_shell_job_is_never_flagged() {
    // Shell jobs are exempt regardless of frequency
    let job = CronJob {
        job_type: JobType::Shell,
        schedule: crate::cron::Schedule::Every {
            every_ms: 60 * 1000, // 1 minute
        },
        ..test_job("echo test")
    };
    assert!(!is_high_frequency_agent_job(&job));
}

#[test]
fn cron_agent_session_path_main_is_stable() {
    assert_eq!(
        cron_agent_session_path(&SessionTarget::Main, "ignored"),
        std::path::PathBuf::from("main")
    );
    assert_eq!(
        cron_agent_session_path(&SessionTarget::Isolated, "abc").to_string_lossy(),
        "cron-abc"
    );
}

#[test]
fn cron_agent_run_security_policy_excludes_scheduler_mutation_tools_by_default() {
    let security = SecurityPolicy::default();
    let mut job = test_job("");
    job.job_type = JobType::Agent;
    job.allowed_tools = None;

    let policy = cron_agent_run_security_policy(&security, &job);

    for tool in [
        "cron_add",
        "cron_update",
        "cron_remove",
        "cron_run",
        "schedule",
    ] {
        assert!(
            !policy.is_tool_allowed(tool),
            "{tool} must be excluded from default cron agent runs"
        );
    }
    assert!(
        policy.is_tool_allowed("http_request"),
        "non-scheduler tools remain available when the base policy is unrestricted"
    );
}

#[test]
fn cron_agent_run_security_policy_respects_explicit_allowed_tools() {
    let security = SecurityPolicy::default();
    let mut job = test_job("");
    job.job_type = JobType::Agent;
    job.allowed_tools = Some(vec!["cron_add".into()]);

    let policy = cron_agent_run_security_policy(&security, &job);

    assert!(
        policy.is_tool_allowed("cron_add"),
        "explicit cron job allowed_tools should remain the override for intentional scheduler automation"
    );
}

#[tokio::test]
#[cfg(not(target_os = "windows"))]
async fn run_job_command_success() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = test_job("echo scheduler-ok");
    let security = test_security(&config);

    let (success, output) = run_job_command(&config, &security, &job)
        .await
        .into_result();
    assert!(success);
    assert!(output.contains("scheduler-ok"));
    assert!(output.contains("status=exit status: 0"));
}

#[tokio::test]
async fn run_manual_job_persists_history_and_broadcasts() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config
        .risk_profiles
        .entry(TEST_AGENT.into())
        .or_default()
        .allowed_commands = vec!["echo".into()];
    let job = cron::add_shell_job_with_approval(
        &config,
        TEST_AGENT,
        Some("manual-run".into()),
        Schedule::Cron {
            expr: "*/5 * * * *".into(),
            tz: None,
        },
        "echo manual-run-ok",
        None,
        true,
    )
    .expect("test job should be persisted");
    let (tx, mut rx) = tokio::sync::broadcast::channel(8);
    let event_tx = Some(tx);

    let result = run_manual_job(&config, &job, CronDeliveryContext::GatewayManual, &event_tx).await;

    assert!(result.success);
    assert_eq!(result.status, "ok");
    assert!(result.output.contains("manual-run-ok"));

    let updated = cron::get_job(&config, &job.id).expect("job state should update");
    assert_eq!(updated.last_status.as_deref(), Some("ok"));
    assert!(
        updated
            .last_output
            .as_deref()
            .is_some_and(|output| output.contains("manual-run-ok"))
    );

    let runs = cron::list_runs(&config, &job.id, 10).expect("run history should list");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "ok");
    assert!(
        runs[0]
            .output
            .as_deref()
            .unwrap_or("")
            .contains("manual-run-ok")
    );

    let event = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("manual trigger should broadcast")
        .expect("broadcast channel should stay open");
    assert_eq!(event["type"], "cron_result");
    assert_eq!(event["job_id"], job.id);
    assert_eq!(event["success"], true);
    assert_eq!(event["manual"], true);
    assert!(
        event["output"]
            .as_str()
            .unwrap_or("")
            .contains("manual-run-ok")
    );
}

#[tokio::test]
#[cfg(not(target_os = "windows"))]
async fn run_job_command_failure() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = test_job("ls definitely_missing_file_for_scheduler_test");
    let security = test_security(&config);

    let (success, output) = run_job_command(&config, &security, &job)
        .await
        .into_result();
    assert!(!success);
    assert!(output.contains("definitely_missing_file_for_scheduler_test"));
    assert!(output.contains("status=exit status:"));
}

#[tokio::test]
#[cfg(not(target_os = "windows"))]
async fn run_job_command_times_out() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config
        .risk_profiles
        .entry(TEST_AGENT.into())
        .or_default()
        .allowed_commands = vec!["sleep".into()];
    let job = test_job("sleep 1");
    let security = test_security(&config);

    let (success, output) =
        run_job_command_with_timeout(&config, &security, &job, Duration::from_millis(50))
            .await
            .into_result();
    assert!(!success);
    assert!(output.contains("job timed out after"));
}

#[tokio::test]
async fn run_job_command_blocks_disallowed_command() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config
        .risk_profiles
        .entry(TEST_AGENT.into())
        .or_default()
        .allowed_commands = vec!["echo".into()];
    let job = test_job("curl https://evil.example");
    let security = test_security(&config);

    let (success, output) = run_job_command(&config, &security, &job)
        .await
        .into_result();
    assert!(!success);
    assert!(output.contains("blocked by security policy"));
    assert!(output.to_lowercase().contains("not allowed"));
}

#[tokio::test]
async fn run_job_command_blocks_forbidden_path_argument() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config
        .risk_profiles
        .entry(TEST_AGENT.into())
        .or_default()
        .allowed_commands = vec!["cat".into()];
    let outside_path = absolute_path_outside_workspace();
    let job = test_job(&format!("cat {outside_path}"));
    let security = test_security(&config);

    let (success, output) = run_job_command(&config, &security, &job)
        .await
        .into_result();
    assert!(!success);
    assert!(output.contains("blocked by security policy"));
    assert!(output.contains("forbidden path argument"));
    assert!(output.contains(outside_path));
}

#[tokio::test]
async fn run_job_command_blocks_forbidden_option_assignment_path_argument() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config
        .risk_profiles
        .entry(TEST_AGENT.into())
        .or_default()
        .allowed_commands = vec!["grep".into()];
    let outside_path = absolute_path_outside_workspace();
    let job = test_job(&format!("grep --file={outside_path} root ./src"));
    let security = test_security(&config);

    let (success, output) = run_job_command(&config, &security, &job)
        .await
        .into_result();
    assert!(!success);
    assert!(output.contains("blocked by security policy"));
    assert!(output.contains("forbidden path argument"));
    assert!(output.contains(outside_path));
}

#[tokio::test]
async fn run_job_command_blocks_forbidden_short_option_attached_path_argument() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config
        .risk_profiles
        .entry(TEST_AGENT.into())
        .or_default()
        .allowed_commands = vec!["grep".into()];
    let outside_path = absolute_path_outside_workspace();
    let job = test_job(&format!("grep -f{outside_path} root ./src"));
    let security = test_security(&config);

    let (success, output) = run_job_command(&config, &security, &job)
        .await
        .into_result();
    assert!(!success);
    assert!(output.contains("blocked by security policy"));
    assert!(output.contains("forbidden path argument"));
    assert!(output.contains(outside_path));
}

#[tokio::test]
#[cfg(not(target_os = "windows"))]
async fn run_job_command_blocks_tilde_user_path_argument() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config
        .risk_profiles
        .entry(TEST_AGENT.into())
        .or_default()
        .allowed_commands = vec!["cat".into()];
    let job = test_job("cat ~root/.ssh/id_rsa");
    let security = test_security(&config);

    let (success, output) = run_job_command(&config, &security, &job)
        .await
        .into_result();
    assert!(!success);
    assert!(output.contains("blocked by security policy"));
    assert!(output.contains("forbidden path argument"));
    assert!(output.contains("~root/.ssh/id_rsa"));
}

#[tokio::test]
#[cfg(not(target_os = "windows"))]
async fn run_job_command_blocks_input_redirection_path_bypass() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config
        .risk_profiles
        .entry(TEST_AGENT.into())
        .or_default()
        .allowed_commands = vec!["cat".into()];
    let job = test_job("cat </etc/passwd");
    let security = test_security(&config);

    let (success, output) = run_job_command(&config, &security, &job)
        .await
        .into_result();
    assert!(!success);
    assert!(output.contains("blocked by security policy"));
    assert!(output.to_lowercase().contains("not allowed"));
}

#[tokio::test]
async fn run_job_command_blocks_readonly_mode() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config
        .risk_profiles
        .entry(TEST_AGENT.into())
        .or_default()
        .level = crate::security::AutonomyLevel::ReadOnly;
    let job = test_job("echo should-not-run");
    let security = test_security(&config);

    let (success, output) = run_job_command(&config, &security, &job)
        .await
        .into_result();
    assert!(!success);
    assert!(output.contains("blocked by security policy"));
    assert!(output.contains("read-only"));
}

#[tokio::test]
async fn run_job_command_blocks_rate_limited() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config
        .runtime_profiles
        .entry(TEST_AGENT.into())
        .or_default()
        .max_actions_per_hour = 0;
    let job = test_job("echo should-not-run");
    let security = test_security(&config);

    let (success, output) = run_job_command(&config, &security, &job)
        .await
        .into_result();
    assert!(!success);
    assert!(output.contains("blocked by security policy"));
    assert!(output.contains("rate limit exceeded"));
}

#[cfg(target_os = "windows")]
fn absolute_path_outside_workspace() -> &'static str {
    r"C:\Windows\win.ini"
}

#[cfg(not(target_os = "windows"))]
fn absolute_path_outside_workspace() -> &'static str {
    "/etc/passwd"
}

#[tokio::test]
#[cfg(not(target_os = "windows"))]
async fn execute_job_with_retry_recovers_after_first_failure() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config.reliability.scheduler_retries = 1;
    config.reliability.provider_backoff_ms = 1;
    config
        .risk_profiles
        .entry(TEST_AGENT.into())
        .or_default()
        .allowed_commands = vec!["sh".into()];
    let security = test_security(&config);

    tokio::fs::write(
        config.data_dir.join("retry-once.sh"),
        "#!/bin/sh\nif [ -f retry-ok.flag ]; then\n  echo recovered\n  exit 0\nfi\ntouch retry-ok.flag\nexit 1\n",
    )
    .await
    .unwrap();
    let job = test_job("sh ./retry-once.sh");

    let (success, output) = Box::pin(execute_job_with_retry(
        &config,
        &security,
        "test-agent",
        &job,
        None,
    ))
    .await;
    assert!(success);
    assert!(output.contains("recovered"));
}

#[tokio::test]
#[cfg(not(target_os = "windows"))]
async fn execute_job_with_retry_exhausts_attempts() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config.reliability.scheduler_retries = 1;
    config.reliability.provider_backoff_ms = 1;
    let security = test_security(&config);

    let job = test_job("ls always_missing_for_retry_test");

    let (success, output) = Box::pin(execute_job_with_retry(
        &config,
        &security,
        "test-agent",
        &job,
        None,
    ))
    .await;
    assert!(!success);
    assert!(output.contains("always_missing_for_retry_test"));
}

#[tokio::test]
async fn run_agent_job_returns_error_without_provider_key() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let mut job = test_job("");
    job.job_type = JobType::Agent;
    job.prompt = Some("Say hello".into());
    let security = test_security(&config);

    let (success, output) =
        Box::pin(run_agent_job(&config, &security, "test-agent", &job, None)).await;
    assert!(!success);
    assert!(output.contains("agent job failed:"));
}

#[tokio::test]
async fn run_agent_job_blocks_readonly_mode() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config
        .risk_profiles
        .entry(TEST_AGENT.into())
        .or_default()
        .level = crate::security::AutonomyLevel::ReadOnly;
    let mut job = test_job("");
    job.job_type = JobType::Agent;
    job.prompt = Some("Say hello".into());
    let security = test_security(&config);

    let (success, output) =
        Box::pin(run_agent_job(&config, &security, "test-agent", &job, None)).await;
    assert!(!success);
    assert!(output.contains("blocked by security policy"));
    assert!(output.contains("read-only"));
}

#[tokio::test]
async fn run_agent_job_blocks_rate_limited() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config
        .runtime_profiles
        .entry(TEST_AGENT.into())
        .or_default()
        .max_actions_per_hour = 0;
    let mut job = test_job("");
    job.job_type = JobType::Agent;
    job.prompt = Some("Say hello".into());
    let security = test_security(&config);

    let (success, output) =
        Box::pin(run_agent_job(&config, &security, "test-agent", &job, None)).await;
    assert!(!success);
    assert!(output.contains("blocked by security policy"));
    assert!(output.contains("rate limit exceeded"));
}

/// Bind a loopback listener that accepts connections but never writes a
/// response, simulating a provider whose HTTP request hangs forever.
async fn spawn_hanging_server() -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let accepts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let accepts_task = std::sync::Arc::clone(&accepts);
    zeroclaw_spawn::spawn!(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            accepts_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            zeroclaw_spawn::spawn!(async move {
                let _stream = stream;
                tokio::time::sleep(Duration::from_secs(30)).await;
            });
        }
    });
    (addr, accepts)
}

/// `test_config` with `TEST_AGENT` repointed at a `custom` provider whose
/// `uri` targets `addr`. `custom` honours `uri` end to end (unlike
/// `openrouter`), so a hung listener can stand in for a hung HTTP call.
async fn test_config_with_hanging_provider(tmp: &TempDir, addr: std::net::SocketAddr) -> Config {
    let mut config = test_config(tmp).await;
    config.providers.models.custom.insert(
        TEST_AGENT.to_string(),
        zeroclaw_config::schema::CustomModelProviderConfig {
            base: zeroclaw_config::schema::ModelProviderConfig {
                uri: Some(format!("http://{addr}")),
                api_key: Some("test-key".to_string()),
                model: Some("test-model".to_string()),
                ..Default::default()
            },
        },
    );
    config.agents.get_mut(TEST_AGENT).unwrap().model_provider =
        format!("custom.{TEST_AGENT}").into();
    config
}

#[tokio::test]
async fn run_agent_job_with_timeout_reports_timeout_for_hung_provider() {
    let tmp = TempDir::new().unwrap();
    let (addr, _accepts) = spawn_hanging_server().await;
    let config = test_config_with_hanging_provider(&tmp, addr).await;
    let mut job = test_job("");
    job.job_type = JobType::Agent;
    job.prompt = Some("Say hello".into());
    let security = test_security(&config);

    let started = std::time::Instant::now();
    let (success, output) = Box::pin(run_agent_job_with_timeout(
        &config,
        &security,
        TEST_AGENT,
        &job,
        Duration::from_millis(750),
        None,
    ))
    .await;
    let elapsed = started.elapsed();

    assert!(!success);
    assert!(
        output.starts_with(CRON_AGENT_JOB_TIMEOUT_PREFIX),
        "unexpected output: {output}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "run_agent_job_with_timeout did not return promptly: {elapsed:?}"
    );
}

#[tokio::test]
async fn execute_and_persist_job_releases_lock_after_agent_timeout() {
    let tmp = TempDir::new().unwrap();
    let (addr, _accepts) = spawn_hanging_server().await;
    let config = test_config_with_hanging_provider(&tmp, addr).await;

    let job = cron::add_agent_job(
        &config,
        TEST_AGENT,
        None,
        crate::cron::Schedule::Cron {
            expr: "*/5 * * * *".into(),
            tz: None,
        },
        "Say hello",
        SessionTarget::Isolated,
        None,
        None,
        false,
        None,
        true,
    )
    .unwrap();

    assert!(
        cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
        "job should be claimable before the run"
    );

    let security = test_security(&config);
    let component = unique_component("agent-timeout-release");
    let (job_id, success, output, _execution_success, _delivery_status) = TEST_AGENT_JOB_TIMEOUT
        .scope(
            Duration::from_millis(750),
            Box::pin(execute_and_persist_job(
                &config, &security, TEST_AGENT, &job, &component, None,
            )),
        )
        .await;

    assert_eq!(job_id, job.id);
    assert!(!success);
    assert!(
        output.starts_with(CRON_AGENT_JOB_TIMEOUT_PREFIX),
        "unexpected output: {output}"
    );
    assert!(
        cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
        "job lock must be released after an agent-run timeout"
    );
}

#[tokio::test]
async fn execute_job_with_retry_does_not_retry_agent_timeout() {
    let tmp = TempDir::new().unwrap();
    let (addr, accepts) = spawn_hanging_server().await;
    let mut config = test_config_with_hanging_provider(&tmp, addr).await;
    // A retryable failure would attempt `retries + 1` times and occupy
    // the slot for that many full timeouts; a non-retryable timeout must
    // attempt exactly once.
    config.reliability.scheduler_retries = 2;
    config.reliability.provider_backoff_ms = 1;

    let mut job = test_job("");
    job.job_type = JobType::Agent;
    job.prompt = Some("Say hello".into());
    let security = test_security(&config);

    let started = std::time::Instant::now();
    let (success, output) = TEST_AGENT_JOB_TIMEOUT
        .scope(
            Duration::from_millis(750),
            Box::pin(execute_job_with_retry(
                &config, &security, TEST_AGENT, &job, None,
            )),
        )
        .await;
    let elapsed = started.elapsed();

    assert!(!success);
    assert!(
        output.starts_with(CRON_AGENT_JOB_TIMEOUT_PREFIX),
        "unexpected output: {output}"
    );
    // Three attempts at 750ms would be ~2.25s plus backoff; one attempt
    // returns near the deadline.
    assert!(
        elapsed < Duration::from_secs(4),
        "timeout was retried (elapsed {elapsed:?})"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let n = accepts.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        n <= 1,
        "timeout path must not retry the hung provider (accepts={n})"
    );
}

#[tokio::test]
async fn queued_and_retried_agent_jobs_keep_their_admitted_task_owner() {
    use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
    use parking_lot::Mutex;
    use zeroclaw_config::schema::{ModelProviderConfig, OllamaModelProviderConfig};
    use zeroclaw_infra::session_sqlite::{SqliteSessionBackend, agent_tasks::TaskOp};
    use zeroclaw_memory::companion as identity;

    #[derive(Clone)]
    struct ProviderState {
        data_dir: std::path::PathBuf,
        store_dir: std::path::PathBuf,
        retry: bool,
        requests: Arc<Mutex<Vec<serde_json::Value>>>,
    }
    async fn chat(
        State(state): State<ProviderState>,
        Json(body): Json<serde_json::Value>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        let has_result = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool");
        let first_request = {
            let mut requests = state.requests.lock();
            requests.push(body);
            requests.len() == 1
        };
        if first_request {
            identity::rename_alias_identity(&state.store_dir, TEST_AGENT, "retired-agent").unwrap();
            identity::invalidate_alias_identity(&state.store_dir, TEST_AGENT).unwrap();
            let replacement =
                identity::resolve_or_mint_agent_identity(&state.store_dir, TEST_AGENT).unwrap();
            SqliteSessionBackend::new(&state.data_dir)
                .unwrap()
                .apply_task_ops(
                    replacement.as_str(),
                    0,
                    &[TaskOp::Add {
                        description: "replacement-only commitment".into(),
                        status: "pending".into(),
                        reminder: None,
                    }],
                )
                .unwrap();
            if state.retry {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": "retry this run"})),
                );
            }
        }
        let message = if first_request || has_result {
            serde_json::json!({"role": "assistant", "content": "done"})
        } else {
            serde_json::json!({"role": "assistant", "content": null, "tool_calls": [{
                "id": "task-write", "type": "function",
                "function": {"name": "task_list", "arguments": serde_json::json!({
                    "expected_revision": 1,
                    "ops": [{"op": "add", "description": "must stay with the admitted owner"}]
                }).to_string()}
            }]})
        };
        (
            StatusCode::OK,
            Json(serde_json::json!({"choices": [{"message": message}]})),
        )
    }

    for (retry, later_poll) in [(false, false), (true, false), (false, true)] {
        let tmp = TempDir::new().unwrap();
        let mut config = test_config(&tmp).await;
        config.scheduler.max_concurrent = 1;
        config.reliability.scheduler_retries = u32::from(retry);
        config.reliability.provider_retries = 0;
        config.reliability.provider_backoff_ms = 1;
        config.memory.backend = "none".into();
        config.memory.auto_save = false;
        let profile = config.risk_profiles.get_mut(TEST_AGENT).unwrap();
        profile.allowed_tools = Some(vec!["task_list".into()]);
        profile.auto_approve = vec!["task_list".into()];
        let store_dir = config.companion_memory.resolved_store_dir(&config.data_dir);
        let admitted = identity::resolve_or_mint_agent_identity(&store_dir, TEST_AGENT).unwrap();
        let sqlite = SqliteSessionBackend::new(&config.data_dir).unwrap();
        sqlite
            .apply_task_ops(
                admitted.as_str(),
                0,
                &[TaskOp::Add {
                    description: "original commitment".into(),
                    status: "pending".into(),
                    reminder: None,
                }],
            )
            .unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/chat/completions", post(chat))
            .with_state(ProviderState {
                data_dir: config.data_dir.clone(),
                store_dir: store_dir.clone(),
                retry,
                requests: requests.clone(),
            });
        let server = zeroclaw_spawn::spawn!(async move {
            axum::serve(listener, app).await.unwrap();
        });
        config.providers.models.ollama.insert(
            "tasks".into(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    uri: Some(format!("http://{addr}")),
                    model: Some("task-model".into()),
                    native_tools: Some(true),
                    timeout_secs: Some(5),
                    ..Default::default()
                },
            },
        );
        config.agents.get_mut(TEST_AGENT).unwrap().model_provider = "ollama.tasks".into();
        let mut jobs = Vec::new();
        for index in 0..if retry || later_poll { 1 } else { 2 } {
            jobs.push(
                cron::add_agent_job(
                    &config,
                    TEST_AGENT,
                    Some(format!("task-owner-{index}")),
                    Schedule::Cron {
                        expr: "*/5 * * * *".into(),
                        tz: None,
                    },
                    "update your commitments",
                    SessionTarget::Isolated,
                    None,
                    None,
                    false,
                    Some(vec!["task_list".into()]),
                    false,
                )
                .unwrap(),
            );
        }
        if retry {
            let (success, output) =
                tokio::time::timeout(Duration::from_secs(15), execute_job_now(&config, &jobs[0]))
                    .await
                    .unwrap();
            assert!(
                success,
                "retry must finish its real task-tool loop: {output}"
            );
        } else {
            // A daemon generation keeps this same snapshot across queued
            // jobs, later DB polls, and worker supervisor restarts.
            let task_identities = TaskIdentity::capture_all(&config);
            let jobs = claim_due_jobs(&config, jobs);
            assert_eq!(jobs.len(), if later_poll { 1 } else { 2 });
            tokio::time::timeout(
                Duration::from_secs(15),
                process_due_jobs(
                    &config,
                    jobs,
                    &unique_component("queued-task-owner"),
                    &None,
                    &task_identities,
                ),
            )
            .await
            .unwrap();
            if later_poll {
                // The original row can remain while an agent rename/delete
                // awaits its memory cascade. Re-read that real SQLite row
                // on a later poll after the provider recreated its alias.
                let jobs = claim_due_jobs(
                    &config,
                    due_jobs(&config, Utc::now() + ChronoDuration::hours(1)).unwrap(),
                );
                assert_eq!(jobs.len(), 1, "the recurring job is selected again");
                tokio::time::timeout(
                    Duration::from_secs(15),
                    process_due_jobs(
                        &config,
                        jobs,
                        &unique_component("later-poll-task-owner"),
                        &None,
                        &task_identities,
                    ),
                )
                .await
                .unwrap();
            }
        }
        let replacement = identity::peek_agent_identity(&store_dir, TEST_AGENT).unwrap();
        assert_ne!(replacement, admitted);
        assert_eq!(
            sqlite.task_list(replacement.as_str()).unwrap().revision,
            1,
            "queued, retried, or later-poll work must not mutate the replacement agent"
        );
        assert_eq!(sqlite.task_list(admitted.as_str()).unwrap().revision, 1);
        let seen = requests.lock();
        assert_eq!(
            seen.len(),
            3,
            "one initial run and one complete task-tool loop"
        );
        assert!(seen[1].to_string().contains("Task list unavailable"));
        assert!(!seen[1].to_string().contains("replacement-only commitment"));
        assert!(seen[2].to_string().contains("identity changed"));
        server.abort();
    }
}

#[tokio::test]
async fn process_due_jobs_marks_component_ok_even_when_idle() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let component = unique_component("scheduler-idle");

    crate::health::mark_component_error(&component, "pre-existing error");
    process_due_jobs(
        &config,
        Vec::new(),
        &component,
        &None,
        &TaskIdentity::capture_all(&config),
    )
    .await;

    let snapshot = crate::health::snapshot_json();
    let entry = &snapshot["components"][component.as_str()];
    assert_eq!(entry["status"], "ok");
    assert!(entry["last_ok"].as_str().is_some());
    assert!(entry["last_error"].is_null());
}

#[tokio::test]
async fn process_due_jobs_failure_does_not_mark_component_unhealthy() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = test_job("ls definitely_missing_file_for_scheduler_component_health_test");
    let component = unique_component("scheduler-fail");

    crate::health::mark_component_ok(&component);
    process_due_jobs(
        &config,
        vec![job],
        &component,
        &None,
        &TaskIdentity::capture_all(&config),
    )
    .await;

    let snapshot = crate::health::snapshot_json();
    let entry = &snapshot["components"][component.as_str()];
    assert_eq!(entry["status"], "ok");
}

#[tokio::test]
async fn persist_job_result_records_run_and_reschedules_shell_job() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = cron::add_job(&config, "test-agent", "*/5 * * * *", "echo ok").unwrap();
    let started = Utc::now();
    let finished = started + ChronoDuration::milliseconds(10);

    let success = persist_job_result(&config, &job, true, "ok", started, finished)
        .await
        .projected_success;
    assert!(success);

    let runs = cron::list_runs(&config, &job.id, 10).unwrap();
    assert_eq!(runs.len(), 1);
    let updated = cron::get_job(&config, &job.id).unwrap();
    assert_eq!(updated.last_status.as_deref(), Some("ok"));
}

#[tokio::test]
async fn persist_job_result_uses_one_write_connection_for_recurring_job() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = cron::add_job(&config, "test-agent", "*/5 * * * *", "echo ok").unwrap();
    let started = Utc::now();
    let finished = started + ChronoDuration::milliseconds(10);

    crate::cron::store::reset_write_connection_count_for_tests(&config);
    let success = persist_job_result(&config, &job, true, "ok", started, finished)
        .await
        .projected_success;

    assert!(success);
    assert_eq!(
        crate::cron::store::write_connection_count_for_tests(&config),
        1
    );
}

#[tokio::test]
async fn persist_job_result_prunes_run_history_and_updates_last_fields() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config.scheduler.max_run_history = 2;
    let job = cron::add_job(&config, "test-agent", "*/5 * * * *", "echo ok").unwrap();
    let base = Utc::now();

    for idx in 0..3 {
        let started = base + ChronoDuration::seconds(idx);
        let finished = started + ChronoDuration::milliseconds(10);
        let output = format!("run-{idx}");

        let success = persist_job_result(&config, &job, true, &output, started, finished)
            .await
            .projected_success;
        assert!(success);
    }

    let runs = cron::list_runs(&config, &job.id, 10).unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].output.as_deref(), Some("run-2"));
    assert_eq!(runs[1].output.as_deref(), Some("run-1"));

    let updated = cron::get_job(&config, &job.id).unwrap();
    assert_eq!(updated.last_status.as_deref(), Some("ok"));
    assert_eq!(updated.last_output.as_deref(), Some("run-2"));
    assert!(updated.last_run.is_some());
}

#[tokio::test]
async fn persist_job_result_rolls_back_run_history_when_job_state_update_fails() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = cron::add_job(&config, "test-agent", "*/5 * * * *", "echo ok").unwrap();
    let original_next_run = job.next_run;
    let started = Utc::now();
    let finished = started + ChronoDuration::milliseconds(10);

    let conn = rusqlite::Connection::open(config.data_dir.join("cron").join("jobs.db")).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_cron_job_update
         BEFORE UPDATE ON cron_jobs
         BEGIN
             SELECT RAISE(ABORT, 'blocked update');
         END;",
    )
    .unwrap();
    drop(conn);

    let success = persist_job_result(&config, &job, true, "ok", started, finished)
        .await
        .projected_success;

    assert!(success);
    assert!(cron::list_runs(&config, &job.id, 10).unwrap().is_empty());

    let stored = cron::get_job(&config, &job.id).unwrap();
    assert_eq!(stored.next_run, original_next_run);
    assert!(stored.last_run.is_none());
    assert!(stored.last_status.is_none());
    assert!(stored.last_output.is_none());
}

#[tokio::test]
async fn persist_job_result_success_deletes_one_shot() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let at = Utc::now() + ChronoDuration::minutes(10);
    let job = cron::add_agent_job(
        &config,
        TEST_AGENT,
        Some("one-shot".into()),
        crate::cron::Schedule::At { at },
        "Hello",
        SessionTarget::Isolated,
        None,
        None,
        true,
        None,
        true,
    )
    .unwrap();
    let started = Utc::now();
    let finished = started + ChronoDuration::milliseconds(10);

    let success = persist_job_result(&config, &job, true, "ok", started, finished)
        .await
        .projected_success;
    assert!(success);
    let lookup = cron::get_job(&config, &job.id);
    assert!(lookup.is_err());
}

#[tokio::test]
async fn persist_job_result_failure_disables_one_shot() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let at = Utc::now() + ChronoDuration::minutes(10);
    let job = cron::add_agent_job(
        &config,
        TEST_AGENT,
        Some("one-shot".into()),
        crate::cron::Schedule::At { at },
        "Hello",
        SessionTarget::Isolated,
        None,
        None,
        true,
        None,
        true,
    )
    .unwrap();
    let started = Utc::now();
    let finished = started + ChronoDuration::milliseconds(10);

    let success = persist_job_result(&config, &job, false, "boom", started, finished)
        .await
        .projected_success;
    assert!(!success);
    let updated = cron::get_job(&config, &job.id).unwrap();
    assert!(!updated.enabled);
    assert_eq!(updated.last_status.as_deref(), Some("error"));
}

#[tokio::test]
async fn persist_job_result_uses_one_write_connection_for_failed_one_shot_disable() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let at = Utc::now() + ChronoDuration::minutes(10);
    let job = cron::add_agent_job(
        &config,
        "test-agent",
        Some("one-shot".into()),
        crate::cron::Schedule::At { at },
        "Hello",
        SessionTarget::Isolated,
        None,
        None,
        true,
        None,
        true,
    )
    .unwrap();
    let started = Utc::now();
    let finished = started + ChronoDuration::milliseconds(10);

    crate::cron::store::reset_write_connection_count_for_tests(&config);
    let success = persist_job_result(&config, &job, false, "boom", started, finished)
        .await
        .projected_success;

    assert!(!success);
    assert_eq!(
        crate::cron::store::write_connection_count_for_tests(&config),
        1
    );
}

#[tokio::test]
async fn persist_job_result_falls_back_to_state_update_when_history_prune_fails() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config.scheduler.max_run_history = 1;
    let job = cron::add_job(&config, "test-agent", "*/5 * * * *", "echo ok").unwrap();
    let original_next_run = job.next_run;
    let seed_started = Utc::now() - ChronoDuration::minutes(20);
    let seed_finished = seed_started + ChronoDuration::milliseconds(10);
    let started = Utc::now();
    let finished = started + ChronoDuration::milliseconds(10);

    let conn = rusqlite::Connection::open(config.data_dir.join("cron").join("jobs.db")).unwrap();
    conn.execute(
        "INSERT INTO cron_runs (job_id, started_at, finished_at, status, output, duration_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            job.id,
            seed_started.to_rfc3339(),
            seed_finished.to_rfc3339(),
            "seed",
            "seed",
            10,
        ],
    )
    .unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_cron_run_prune
         BEFORE DELETE ON cron_runs
         BEGIN
             SELECT RAISE(ABORT, 'blocked prune');
         END;",
    )
    .unwrap();
    drop(conn);

    let success = persist_job_result(&config, &job, true, "ok", started, finished)
        .await
        .projected_success;
    assert!(success);

    let runs = cron::list_runs(&config, &job.id, 10).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "seed");

    let updated = cron::get_job(&config, &job.id).unwrap();
    assert_eq!(updated.last_status.as_deref(), Some("ok"));
    assert_eq!(updated.last_output.as_deref(), Some("ok"));
    assert!(updated.last_run.is_some());
    assert!(updated.next_run >= original_next_run);
}

#[tokio::test]
async fn persist_job_result_falls_back_to_disable_when_auto_delete_history_insert_fails() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let at = Utc::now() + ChronoDuration::minutes(10);
    let job = cron::add_once_at(&config, "test-agent", at, "echo one-shot-shell").unwrap();
    assert!(job.delete_after_run);
    let started = Utc::now();
    let finished = started + ChronoDuration::milliseconds(10);

    let conn = rusqlite::Connection::open(config.data_dir.join("cron").join("jobs.db")).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_cron_run_insert
         BEFORE INSERT ON cron_runs
         BEGIN
             SELECT RAISE(ABORT, 'blocked insert');
         END;",
    )
    .unwrap();
    drop(conn);

    let success = persist_job_result(&config, &job, true, "ok", started, finished)
        .await
        .projected_success;
    assert!(success);

    let updated = cron::get_job(&config, &job.id).unwrap();
    assert!(!updated.enabled);
    assert_eq!(updated.last_status.as_deref(), Some("ok"));
    assert_eq!(updated.last_output.as_deref(), Some("ok"));
    assert!(cron::list_runs(&config, &job.id, 10).unwrap().is_empty());
}

#[tokio::test]
async fn persist_job_result_success_deletes_one_shot_shell_job() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let at = Utc::now() + ChronoDuration::minutes(10);
    let job = cron::add_once_at(&config, "test-agent", at, "echo one-shot-shell").unwrap();
    assert!(job.delete_after_run);
    let started = Utc::now();
    let finished = started + ChronoDuration::milliseconds(10);

    let success = persist_job_result(&config, &job, true, "ok", started, finished)
        .await
        .projected_success;
    assert!(success);
    let lookup = cron::get_job(&config, &job.id);
    assert!(lookup.is_err());
}

#[tokio::test]
async fn persist_job_result_failure_disables_one_shot_shell_job() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let at = Utc::now() + ChronoDuration::minutes(10);
    let job = cron::add_once_at(&config, "test-agent", at, "echo one-shot-shell").unwrap();
    assert!(job.delete_after_run);
    let started = Utc::now();
    let finished = started + ChronoDuration::milliseconds(10);

    let success = persist_job_result(&config, &job, false, "boom", started, finished)
        .await
        .projected_success;
    assert!(!success);
    let updated = cron::get_job(&config, &job.id).unwrap();
    assert!(!updated.enabled);
    assert_eq!(updated.last_status.as_deref(), Some("error"));
}

#[tokio::test]
async fn persist_job_result_delivery_stubbed_succeeds() {
    // Delivery is stubbed (moved to zeroclaw-channels orchestrator).
    // This test verifies the stub returns Ok, so persist_job_result succeeds.
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = cron::add_agent_job(
        &config,
        TEST_AGENT,
        Some("announce-job".into()),
        crate::cron::Schedule::Cron {
            expr: "*/5 * * * *".into(),
            tz: None,
        },
        "deliver this",
        SessionTarget::Isolated,
        None,
        Some(DeliveryConfig {
            mode: "announce".into(),
            channel: Some("telegram".into()),
            to: Some("123456".into()),
            thread_id: None,
            best_effort: false,
        }),
        false,
        None,
        true,
    )
    .unwrap();
    let started = Utc::now();
    let finished = started + ChronoDuration::milliseconds(10);

    let success = persist_job_result(&config, &job, true, "ok", started, finished)
        .await
        .projected_success;
    assert!(success);

    let updated = cron::get_job(&config, &job.id).unwrap();
    assert!(updated.enabled);
    assert_eq!(updated.last_status.as_deref(), Some("ok"));

    let runs = cron::list_runs(&config, &job.id, 10).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "ok");
}

#[tokio::test]
async fn persist_job_result_delivery_failure_best_effort_marks_degraded() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    register_recording_delivery_fn();
    let mut job = cron::add_job(&config, "test-agent", "*/5 * * * *", "echo ok").unwrap();
    job.delivery = DeliveryConfig {
        mode: "announce".into(),
        channel: Some("fail-delivery".into()),
        to: Some("123456".into()),
        thread_id: None,
        best_effort: true,
    };
    let started = Utc::now();
    let finished = started + ChronoDuration::milliseconds(10);

    let success = persist_job_result(&config, &job, true, "ok", started, finished)
        .await
        .projected_success;
    assert!(success);

    let updated = cron::get_job(&config, &job.id).unwrap();
    assert!(updated.enabled);
    assert_eq!(updated.last_status.as_deref(), Some("degraded"));
    assert!(
        updated
            .last_output
            .as_deref()
            .unwrap_or_default()
            .contains("delivery failed:")
    );

    let runs = cron::list_runs(&config, &job.id, 10).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "degraded");
}

#[tokio::test]
async fn delivery_failure_classification_preserves_empty_output_evidence() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    register_recording_delivery_fn();
    let mut job = cron::add_job(&config, "test-agent", "*/5 * * * *", "echo ok").unwrap();
    job.delivery = DeliveryConfig {
        mode: "announce".into(),
        channel: Some("fail-delivery".into()),
        to: Some("123456".into()),
        thread_id: None,
        best_effort: true,
    };

    let outcome = deliver_and_classify_run_result(
        &config,
        &job,
        true,
        String::new(),
        CronDeliveryContext::Scheduled,
        Utc::now(),
    )
    .await;

    assert!(outcome.success);
    assert_eq!(outcome.status, "degraded");
    assert!(outcome.output.starts_with("delivery failed:"));
}

#[tokio::test]
async fn persist_job_result_at_schedule_without_delete_after_run_is_disabled() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let at = Utc::now() + ChronoDuration::minutes(10);
    let job = cron::add_agent_job(
        &config,
        TEST_AGENT,
        Some("at-no-autodelete".into()),
        crate::cron::Schedule::At { at },
        "Hello",
        SessionTarget::Isolated,
        None,
        None,
        false,
        None,
        true,
    )
    .unwrap();
    assert!(!job.delete_after_run);

    let started = Utc::now();
    let finished = started + ChronoDuration::milliseconds(10);
    let success = persist_job_result(&config, &job, true, "ok", started, finished)
        .await
        .projected_success;
    assert!(success);

    // After reschedule_after_run, At schedule jobs should be disabled
    // to prevent re-execution with a past next_run timestamp.
    let updated = cron::get_job(&config, &job.id).unwrap();
    assert!(
        !updated.enabled,
        "At schedule job should be disabled after execution via reschedule"
    );
    assert_eq!(updated.last_status.as_deref(), Some("ok"));
}

#[tokio::test]
async fn deliver_if_configured_handles_none_mode() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = test_job("echo ok");

    // Default delivery mode is not "announce", so should be a no-op.
    assert!(
        deliver_if_configured(&config, &job, "x", "test-run")
            .await
            .is_ok()
    );
}

static DELIVERED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Channel name the recorder counts. Used only by the suppression test.
const COUNT_CHANNEL: &str = "count-delivery";

fn register_recording_delivery_fn() {
    // Idempotent: register_delivery_fn is a no-op once the OnceLock is set,
    // so repeated calls across tests are safe and the first writer wins. The
    // handler honours the `fail-delivery` failure contract used by the
    // delivery-classification tests so it composes regardless of order.
    register_delivery_fn(Box::new(|_config, channel, _target, _thread, _output| {
        Box::pin(async move {
            if channel == "fail-delivery" {
                anyhow::bail!("synthetic delivery failure");
            }
            if channel == COUNT_CHANNEL {
                DELIVERED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        })
    }));
}

fn announce_job() -> CronJob {
    let mut job = test_job("echo ok");
    job.delivery = DeliveryConfig {
        mode: "announce".to_string(),
        channel: Some(COUNT_CHANNEL.to_string()),
        to: Some("chat-id".to_string()),
        thread_id: None,
        best_effort: true,
    };
    job
}

#[tokio::test]
async fn deliver_if_configured_suppresses_no_reply_but_delivers_real_and_failure() {
    register_recording_delivery_fn();
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = announce_job();
    use std::sync::atomic::Ordering::SeqCst;

    // Quiet sentinel forms must NOT trigger delivery.
    for quiet in [
        "NO_REPLY",
        "NO_REPLY: nothing to report",
        "NO_REPLY[INFO]: healthy",
    ] {
        let before = DELIVERED.load(SeqCst);
        deliver_if_configured(&config, &job, quiet, "test-run")
            .await
            .unwrap();
        assert_eq!(
            DELIVERED.load(SeqCst),
            before,
            "quiet sentinel {quiet:?} must be suppressed (no delivery)"
        );
    }

    // Real content must be delivered.
    let before = DELIVERED.load(SeqCst);
    deliver_if_configured(&config, &job, "All systems nominal", "test-run")
        .await
        .unwrap();
    assert_eq!(
        DELIVERED.load(SeqCst),
        before + 1,
        "real content must be delivered"
    );

    // Failure / refusal kinds must be delivered (operator-visible).
    for visible in [
        "NO_REPLY[FAIL]: database check timed out",
        "NO_REPLY[REFUSE]: policy prevented the check",
    ] {
        let before = DELIVERED.load(SeqCst);
        deliver_if_configured(&config, &job, visible, "test-run")
            .await
            .unwrap();
        assert_eq!(
            DELIVERED.load(SeqCst),
            before + 1,
            "failure/refusal kind {visible:?} must be delivered, not suppressed"
        );
    }
}

#[test]
fn heartbeat_announce_decision_matches_worker_behavior() {
    // NO_REPLY heartbeat: suppressed.
    assert!(!announce_delivery_decision("NO_REPLY").should_deliver());
    assert!(!announce_delivery_decision("NO_REPLY[INFO]: all good").should_deliver());
    // Non-sentinel heartbeat output: delivered.
    assert!(announce_delivery_decision("disk usage 42%").should_deliver());
    // Empty-output fallback string the worker builds: must deliver.
    assert!(
        announce_delivery_decision("💓 heartbeat task completed: db health").should_deliver(),
        "the empty-output heartbeat fallback must never be mistaken for a sentinel"
    );
    // Failure/refusal kinds: delivered (operator-visible).
    assert!(announce_delivery_decision("NO_REPLY[FAIL]: db timed out").should_deliver());
    assert!(announce_delivery_decision("NO_REPLY[REFUSE]: blocked by policy").should_deliver());
}

#[tokio::test]
async fn deliver_announcement_returns_ok_when_no_handler_registered() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    // No registered handler is a runtime-level state, not a delivery
    // failure. The caller (persist_job_result) should record the job
    // execution as successful; the missing handler is logged via
    // tracing::warn for operator visibility.
    deliver_announcement(&config, "telegram", "chat-id", None, "payload")
        .await
        .expect("missing delivery handler should be Ok with a warn log");
}

#[tokio::test]
async fn delivery_to_a_configured_bridge_is_queued_in_its_outbox() {
    register_recording_delivery_fn();
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config.gateway.bridges.insert(
        COUNT_CHANNEL.to_string(),
        zeroclaw_config::schema::GatewayBridgeConfig {
            token_hash: "0f".repeat(32),
            ..Default::default()
        },
    );
    let mut job = announce_job();
    job.delivery.to = Some("4242".into());
    job.delivery.thread_id = Some("7".into());
    use std::sync::atomic::Ordering::SeqCst;
    let before = DELIVERED.load(SeqCst);

    let started_at = Utc::now();
    let outcome = deliver_and_classify_run_result(
        &config,
        &job,
        true,
        "digest ready".to_string(),
        CronDeliveryContext::Scheduled,
        started_at,
    )
    .await;

    assert_eq!(outcome.delivery_status, "accepted");
    // A bridge name shadows the in-core channel: the registered channel
    // delivery function is not called.
    assert_eq!(DELIVERED.load(SeqCst), before);
    let outbox = zeroclaw_infra::bridge_outbox::BridgeOutbox::shared(&config.data_dir).unwrap();
    let queued = outbox.pending(COUNT_CHANNEL, 0, 10).unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].to, "4242");
    assert_eq!(queued[0].thread_id.as_deref(), Some("7"));
    assert_eq!(queued[0].content, "digest ready");
    assert_eq!(queued[0].source_kind, "cron");
    assert_eq!(queued[0].source_id, job.id);
    assert_eq!(queued[0].event_id, started_at.to_rfc3339());
    let repeated = deliver_and_classify_run_result(
        &config,
        &job,
        true,
        "digest ready".into(),
        CronDeliveryContext::Scheduled,
        started_at,
    )
    .await;
    assert_eq!(repeated.delivery_status, "accepted");
    assert_eq!(outbox.pending(COUNT_CHANNEL, 0, 10).unwrap(), queued);
}

#[tokio::test]
async fn cron_output_with_an_api_key_is_redacted_in_the_bridge_outbox() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config.gateway.bridges.insert(
        "tg".to_string(),
        zeroclaw_config::schema::GatewayBridgeConfig {
            token_hash: "0f".repeat(32),
            ..Default::default()
        },
    );
    let key = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789ABCD";
    let mut job = announce_job();
    job.delivery.channel = Some("tg".into());
    job.delivery.to = Some("4242".into());

    let outcome = deliver_and_classify_run_result(
        &config,
        &job,
        true,
        format!("env dump: ANTHROPIC_API_KEY={key}"),
        CronDeliveryContext::Scheduled,
        Utc::now(),
    )
    .await;

    assert_eq!(outcome.delivery_status, "accepted");
    let outbox = zeroclaw_infra::bridge_outbox::BridgeOutbox::shared(&config.data_dir).unwrap();
    let queued = outbox.pending("tg", 0, 10).unwrap();
    assert_eq!(queued.len(), 1);
    assert!(!queued[0].content.contains(key), "{}", queued[0].content);
    assert!(queued[0].content.starts_with("env dump: "));
    assert!(
        queued[0].content.contains("[REDACTED"),
        "{}",
        queued[0].content
    );
}

#[test]
fn build_cron_shell_command_uses_sh_non_login() {
    let workspace = std::env::temp_dir();
    let cmd = build_cron_shell_command("echo cron-test", &workspace).unwrap();
    let debug = format!("{cmd:?}");
    assert!(debug.contains("echo cron-test"));
    assert!(debug.contains("\"sh\""), "should use sh: {debug}");
    // Must NOT use login shell (-l) — login shells load full profile
    // and are slow/unpredictable for cron jobs.
    assert!(
        !debug.contains("\"-lc\""),
        "must not use login shell: {debug}"
    );
}

#[tokio::test]
async fn build_cron_shell_command_executes_successfully() {
    let workspace = std::env::temp_dir();
    let mut cmd = build_cron_shell_command("echo cron-ok", &workspace).unwrap();
    let output = cmd.output().await.unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cron-ok"));
}

#[tokio::test]
async fn catch_up_queries_all_overdue_jobs_ignoring_max_tasks() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    config.scheduler.max_tasks = 1; // limit normal polling to 1

    // Create 3 jobs with "every minute" schedule
    for i in 0..3 {
        let _ = cron::add_job(
            &config,
            "test-agent",
            "* * * * *",
            &format!("echo catchup-{i}"),
        )
        .unwrap();
    }

    // Verify normal due_jobs is limited to max_tasks=1
    let far_future = Utc::now() + ChronoDuration::days(1);
    let due = cron::due_jobs(&config, far_future).unwrap();
    assert_eq!(due.len(), 1, "due_jobs must respect max_tasks");

    // all_overdue_jobs ignores the limit
    let overdue = cron::all_overdue_jobs(&config, far_future).unwrap();
    assert_eq!(overdue.len(), 3, "all_overdue_jobs must return all");
}

// scan_and_redact_output tests moved to zeroclaw-channels orchestrator

// ── Broadcast / EventBroadcast tests ─────────────────────────────

#[tokio::test]
async fn broadcast_sends_cron_result_on_success() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    let job = test_job("echo broadcast-ok");
    // Bind the synthetic test job to test-agent so process_due_jobs's
    // owning-agent lookup succeeds (jobs without an owner are skipped).
    config
        .agents
        .get_mut("test-agent")
        .unwrap()
        .cron_jobs
        .push(job.id.clone());
    let component = unique_component("broadcast-ok");

    let (tx, mut rx) = tokio::sync::broadcast::channel::<serde_json::Value>(16);
    let event_tx: EventBroadcast = Some(tx);

    process_due_jobs(
        &config,
        vec![job],
        &component,
        &event_tx,
        &TaskIdentity::capture_all(&config),
    )
    .await;

    let event = rx.try_recv().expect("should receive a broadcast event");
    assert_eq!(event["type"], "cron_result");
    assert_eq!(event["job_id"], "test-job");
    assert_eq!(event["success"], true);
    assert!(event["output"].as_str().unwrap().contains("broadcast-ok"));
    assert!(event["timestamp"].as_str().is_some());
}

#[tokio::test]
async fn broadcast_sends_cron_result_on_failure() {
    let tmp = TempDir::new().unwrap();
    let mut config = test_config(&tmp).await;
    let job = test_job("ls definitely_missing_file_for_broadcast_fail_test");
    config
        .agents
        .get_mut("test-agent")
        .unwrap()
        .cron_jobs
        .push(job.id.clone());
    let component = unique_component("broadcast-fail");

    let (tx, mut rx) = tokio::sync::broadcast::channel::<serde_json::Value>(16);
    let event_tx: EventBroadcast = Some(tx);

    process_due_jobs(
        &config,
        vec![job],
        &component,
        &event_tx,
        &TaskIdentity::capture_all(&config),
    )
    .await;

    let event = rx.try_recv().expect("should receive a broadcast event");
    assert_eq!(event["type"], "cron_result");
    assert_eq!(event["job_id"], "test-job");
    assert_eq!(event["success"], false);
    assert!(event["timestamp"].as_str().is_some());
}

#[tokio::test]
async fn claim_due_jobs_skips_in_flight_job() {
    // once a due job is claimed for execution, a
    // subsequent selection pass must not pick it up again until the prior
    // run releases it — otherwise a job that runs longer than the poll
    // interval is launched repeatedly.
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = cron::add_job(&config, TEST_AGENT, "*/5 * * * *", "echo ok").unwrap();

    let claimed = claim_due_jobs(&config, vec![job.clone()]);
    assert_eq!(claimed.len(), 1, "first selection claims the job");

    let claimed_again = claim_due_jobs(&config, vec![job.clone()]);
    assert!(
        claimed_again.is_empty(),
        "an in-flight job must be skipped by the next selection pass"
    );

    cron::release_job(&config, &job.id).unwrap();
    let after_release = claim_due_jobs(&config, vec![job]);
    assert_eq!(
        after_release.len(),
        1,
        "after release the job is selectable again"
    );
}

#[tokio::test]
async fn process_due_jobs_releases_lock_for_skipped_orphan_job() {
    // A job claimed for execution but then skipped by process_due_jobs (here
    // an orphan with no owning agent) must have its in-flight lock released,
    // so it is retried on the next poll instead of being wedged out of
    // due_jobs until restart
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    // Insert a real, claimable DB row under a configured agent, then drive
    // process_due_jobs with an in-memory view whose agent_alias is cleared.
    // With an empty alias and an id bound to no [agents.<x>].cron_jobs list,
    // resolve_owning_agent returns None, so the job is skipped as an orphan.
    let job = cron::add_job(&config, TEST_AGENT, "* * * * *", "echo orphan").unwrap();
    assert!(cron::claim_job(&config, &job.id, Utc::now()).unwrap());
    let orphan = CronJob {
        agent_alias: String::new(),
        ..job.clone()
    };

    process_due_jobs(
        &config,
        vec![orphan],
        &unique_component("orphan"),
        &None,
        &TaskIdentity::capture_all(&config),
    )
    .await;

    assert!(
        cron::claim_job(&config, &job.id, Utc::now()).unwrap(),
        "a skipped orphan job's in-flight lock must be released, not leaked"
    );
}

#[tokio::test]
async fn broadcast_none_skips_without_error() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = test_job("echo no-broadcast");
    let component = unique_component("broadcast-none");

    // event_tx = None — should complete without panic.
    process_due_jobs(
        &config,
        vec![job],
        &component,
        &None,
        &TaskIdentity::capture_all(&config),
    )
    .await;
}

#[tokio::test]
async fn broadcast_handles_no_subscribers() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = test_job("echo no-subscribers");
    let component = unique_component("broadcast-no-sub");

    let (tx, _) = tokio::sync::broadcast::channel::<serde_json::Value>(16);
    // Drop the only receiver immediately — `let _ = tx.send(...)` in
    // process_due_jobs must not panic when there are no subscribers.
    let event_tx: EventBroadcast = Some(tx);

    process_due_jobs(
        &config,
        vec![job],
        &component,
        &event_tx,
        &TaskIdentity::capture_all(&config),
    )
    .await;
    // If we got here without panic, the test passes.
}

// ── #64: execution/delivery truth-separation tests ──

fn delivery_job(channel: Option<&str>, best_effort: bool) -> CronJob {
    let mut job = test_job("echo ok");
    job.delivery = DeliveryConfig {
        mode: "announce".to_string(),
        channel: channel.map(str::to_string),
        to: Some("chat-id".to_string()),
        thread_id: None,
        best_effort,
    };
    job
}

#[tokio::test]
async fn classify_execution_success_delivery_success() {
    register_recording_delivery_fn();
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = delivery_job(Some(COUNT_CHANNEL), true);

    let outcome = deliver_and_classify_run_result(
        &config,
        &job,
        true,
        "all good".to_string(),
        CronDeliveryContext::Scheduled,
        Utc::now(),
    )
    .await;

    assert!(outcome.execution_success, "execution must remain success");
    assert_eq!(outcome.delivery_status, "succeeded");
    assert_eq!(outcome.status, "ok");
    assert!(outcome.success, "compatibility success should be true");
}

#[tokio::test]
async fn classify_execution_success_delivery_not_requested() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let mut job = test_job("echo ok");
    job.delivery = DeliveryConfig {
        mode: "none".to_string(),
        ..Default::default()
    };

    let outcome = deliver_and_classify_run_result(
        &config,
        &job,
        true,
        "all good".to_string(),
        CronDeliveryContext::Scheduled,
        Utc::now(),
    )
    .await;

    assert!(outcome.execution_success);
    assert_eq!(outcome.delivery_status, "not_requested");
    assert_eq!(outcome.status, "ok");
    assert!(outcome.success);
}

#[tokio::test]
async fn classify_execution_success_delivery_suppressed_no_reply() {
    register_recording_delivery_fn();
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = delivery_job(Some(COUNT_CHANNEL), true);

    let outcome = deliver_and_classify_run_result(
        &config,
        &job,
        true,
        "NO_REPLY".to_string(),
        CronDeliveryContext::Scheduled,
        Utc::now(),
    )
    .await;

    assert!(outcome.execution_success);
    assert_eq!(outcome.delivery_status, "suppressed");
    assert_eq!(outcome.status, "ok");
    assert!(outcome.success);
}

#[tokio::test]
async fn classify_execution_success_delivery_fails_best_effort() {
    register_recording_delivery_fn();
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = delivery_job(Some("fail-delivery"), true);

    let outcome = deliver_and_classify_run_result(
        &config,
        &job,
        true,
        "executed ok".to_string(),
        CronDeliveryContext::Scheduled,
        Utc::now(),
    )
    .await;

    // Critical invariant: execution truth is NOT rewritten by delivery failure.
    assert!(outcome.execution_success, "execution must remain success");
    assert_eq!(outcome.delivery_status, "failed");
    // Best-effort: compatibility success stays true, status is degraded.
    assert!(
        outcome.success,
        "best_effort delivery failure should not negate success"
    );
    assert_eq!(outcome.status, "degraded");
    assert!(outcome.output.contains("delivery failed"));
}

#[tokio::test]
async fn classify_execution_success_delivery_fails_strict() {
    register_recording_delivery_fn();
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = delivery_job(Some("fail-delivery"), false);

    let outcome = deliver_and_classify_run_result(
        &config,
        &job,
        true,
        "executed ok".to_string(),
        CronDeliveryContext::Scheduled,
        Utc::now(),
    )
    .await;

    // Critical invariant: execution truth is NOT rewritten by delivery failure.
    assert!(outcome.execution_success, "execution must remain success");
    assert_eq!(outcome.delivery_status, "failed");
    // Strict: compatibility projection fails (policy-level), but component
    // truth in the outcome still shows execution succeeded.
    assert!(
        !outcome.success,
        "strict delivery failure should project failure"
    );
    assert_eq!(outcome.status, "error");
}

#[tokio::test]
async fn classify_execution_failure_and_delivery_failure_both_visible() {
    register_recording_delivery_fn();
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = delivery_job(Some("fail-delivery"), false);

    let outcome = deliver_and_classify_run_result(
        &config,
        &job,
        false,
        "executed with error".to_string(),
        CronDeliveryContext::Scheduled,
        Utc::now(),
    )
    .await;

    // Both failures remain separately visible — delivery error does not
    // replace execution error.
    assert!(
        !outcome.execution_success,
        "execution failure must be visible"
    );
    assert_eq!(outcome.delivery_status, "failed");
    assert!(!outcome.success);
    assert_eq!(outcome.status, "error");
    // Both errors should be in the output.
    assert!(outcome.output.contains("executed with error"));
    assert!(outcome.output.contains("delivery failed"));
}

#[tokio::test]
async fn persisted_run_carries_component_truth_after_delivery_failure() {
    register_recording_delivery_fn();
    let tmp = TempDir::new().unwrap();
    let config = test_config(&tmp).await;
    let job = cron::add_agent_job(
        &config,
        TEST_AGENT,
        Some("delivery-truth-test".into()),
        crate::cron::Schedule::Cron {
            expr: "*/5 * * * *".into(),
            tz: None,
        },
        "deliver this",
        SessionTarget::Isolated,
        None,
        Some(DeliveryConfig {
            mode: "announce".into(),
            channel: Some("fail-delivery".into()),
            to: Some("chat-id".into()),
            thread_id: None,
            best_effort: true,
        }),
        false,
        None,
        true,
    )
    .unwrap();
    let started = Utc::now();
    let finished = started + ChronoDuration::milliseconds(10);

    let _ = persist_job_result(&config, &job, true, "executed ok", started, finished).await;

    let runs = cron::list_runs(&config, &job.id, 10).unwrap();
    assert_eq!(runs.len(), 1);

    // The stored run must show execution succeeded even though delivery failed.
    assert_eq!(
        runs[0].execution_status.as_deref(),
        Some("success"),
        "persisted execution_status must be success"
    );
    assert_eq!(
        runs[0].delivery_status.as_deref(),
        Some("failed"),
        "persisted delivery_status must be failed"
    );
    // Compatibility status is degraded.
    assert_eq!(runs[0].status, "degraded");
}
