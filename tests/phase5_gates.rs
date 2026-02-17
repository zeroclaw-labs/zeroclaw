use anyhow::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::TempDir;
use zeroclaw::db::Registry;
use zeroclaw::migrate;

/// Create a minimal OpenClaw config JSON with the given agents, bindings, and channels.
fn make_openclaw_config(
    agents_json: &str,
    bindings_json: &str,
    channels_json: &str,
    extra_fields: &str,
) -> String {
    format!(
        r#"{{
  "agents": {{
    "defaults": {{
      "model": {{ "primary": "anthropic/claude-sonnet-4-20250514" }},
      "workspace": "/tmp/test-workspace",
      "heartbeat": {{ "every": "30m" }}
    }},
    "list": [{agents_json}]
  }},
  "bindings": [{bindings_json}],
  "channels": {{{channels_json}}}
  {extra_fields}
}}"#
    )
}

/// Create a models.json for an agent.
fn create_models_json(dir: &Path, provider: &str, api_key: &str) {
    let models = format!(
        r#"{{ "providers": {{ "{provider}": {{ "apiKey": "{api_key}" }} }} }}"#
    );
    fs::write(dir.join("models.json"), models).unwrap();
}

/// Set up a two-agent OpenClaw structure in a temp directory.
fn setup_two_agent_env(
    tmp: &TempDir,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let openclaw_dir = tmp.path().join("openclaw");
    fs::create_dir_all(&openclaw_dir).unwrap();

    // Agent directories
    let agent1_dir = openclaw_dir.join("agents").join("main").join("agent");
    let agent2_dir = openclaw_dir.join("agents").join("helper").join("agent");
    fs::create_dir_all(&agent1_dir).unwrap();
    fs::create_dir_all(&agent2_dir).unwrap();

    create_models_json(&agent1_dir, "anthropic", "sk-ant-main-key-12345678");
    create_models_json(&agent2_dir, "anthropic", "sk-ant-helper-key-87654321");

    // Workspace dirs
    let ws1 = tmp.path().join("ws-main");
    let ws2 = tmp.path().join("ws-helper");
    fs::create_dir_all(&ws1).unwrap();
    fs::create_dir_all(&ws2).unwrap();

    let config_json = make_openclaw_config(
        &format!(
            r#"
        {{ "id": "main", "workspace": "{}" }},
        {{ "id": "helper", "workspace": "{}" }}
        "#,
            ws1.display(),
            ws2.display()
        ),
        r#"
        { "agentId": "main", "match": { "channel": "telegram" }, "account": "bot1" },
        { "agentId": "helper", "match": { "channel": "telegram" }, "account": "bot2" }
        "#,
        r#"
        "telegram": {
            "accounts": {
                "bot1": { "botToken": "111:AAA-main-bot-token-long", "allowFrom": ["user_a"] },
                "bot2": { "botToken": "222:BBB-helper-bot-token-long", "allowFrom": ["user_b", "user_c"] }
            }
        }
        "#,
        "",
    );

    let config_path = openclaw_dir.join("openclaw.json");
    fs::write(&config_path, config_json).unwrap();

    let cp_dir = tmp.path().join("cp");
    let instances_dir = cp_dir.join("instances");
    fs::create_dir_all(&instances_dir).unwrap();

    (config_path, cp_dir, instances_dir)
}

// ── Gate 1: Two-agent import ────────────────────────────────────

#[test]
fn gate1_two_agent_import() -> Result<()> {
    let tmp = TempDir::new()?;
    let (config_path, cp_dir, instances_dir) = setup_two_agent_env(&tmp);

    let registry = Registry::open(&cp_dir.join("registry.db"))?;

    let report = migrate::openclaw::run_openclaw_migration(
        &config_path,
        false,
        &cp_dir,
        &registry,
        &instances_dir,
    )?;

    assert!(report.errors.is_empty(), "Errors: {:?}", report.errors);
    assert_eq!(report.created.len(), 2, "Should create 2 instances");

    // Verify instances in DB
    let instances = registry.list_instances()?;
    assert_eq!(instances.len(), 2);

    // Both should be stopped
    for inst in &instances {
        assert_eq!(inst.status, "stopped");
    }

    // Distinct ports
    let ports: Vec<u16> = instances.iter().map(|i| i.port).collect();
    assert_ne!(ports[0], ports[1], "Ports must be distinct");

    // Names match agent IDs
    let names: Vec<&str> = instances.iter().map(|i| i.name.as_str()).collect();
    assert!(names.contains(&"main"));
    assert!(names.contains(&"helper"));

    Ok(())
}

// ── Gate 2: Channel binding correct ─────────────────────────────

#[test]
fn gate2_channel_binding_correct() -> Result<()> {
    let tmp = TempDir::new()?;
    let (config_path, cp_dir, instances_dir) = setup_two_agent_env(&tmp);

    let registry = Registry::open(&cp_dir.join("registry.db"))?;

    let report = migrate::openclaw::run_openclaw_migration(
        &config_path,
        false,
        &cp_dir,
        &registry,
        &instances_dir,
    )?;

    assert!(report.errors.is_empty(), "Errors: {:?}", report.errors);

    // Parse each created config TOML and verify telegram bindings
    for entry in &report.created {
        let config_toml_path = instances_dir
            .join(&entry.instance_id)
            .join("config.toml");
        let content = fs::read_to_string(&config_toml_path)?;
        let parsed: toml::Value = toml::from_str(&content)?;

        if entry.agent_id == "main" {
            let tg = &parsed["channels_config"]["telegram"];
            assert_eq!(tg["bot_token"].as_str().unwrap(), "111:AAA-main-bot-token-long");
            let users: Vec<&str> = tg["allowed_users"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert_eq!(users, vec!["user_a"]);
        } else if entry.agent_id == "helper" {
            let tg = &parsed["channels_config"]["telegram"];
            assert_eq!(tg["bot_token"].as_str().unwrap(), "222:BBB-helper-bot-token-long");
            let users: Vec<&str> = tg["allowed_users"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert_eq!(users, vec!["user_b", "user_c"]);
        }
    }

    Ok(())
}

// ── Gate 3: Unsupported field warnings ──────────────────────────

#[test]
fn gate3_unsupported_field_warnings() -> Result<()> {
    let tmp = TempDir::new()?;
    let openclaw_dir = tmp.path().join("openclaw");
    fs::create_dir_all(&openclaw_dir)?;

    let agent_dir = openclaw_dir.join("agents").join("a1").join("agent");
    fs::create_dir_all(&agent_dir)?;
    create_models_json(&agent_dir, "anthropic", "sk-ant-test-key-12345678");

    let ws = tmp.path().join("ws");
    fs::create_dir_all(&ws)?;

    let config = make_openclaw_config(
        &format!(r#"{{ "id": "a1", "workspace": "{}" }}"#, ws.display()),
        "",
        "",
        r#", "tools": [], "plugins": {}, "commands": [], "skills": {}"#,
    );

    let config_path = openclaw_dir.join("openclaw.json");
    fs::write(&config_path, config)?;

    let cp_dir = tmp.path().join("cp");
    let instances_dir = cp_dir.join("instances");
    fs::create_dir_all(&instances_dir)?;

    let registry = Registry::open(&cp_dir.join("registry.db"))?;
    let report = migrate::openclaw::run_openclaw_migration(
        &config_path,
        true,
        &cp_dir,
        &registry,
        &instances_dir,
    )?;

    assert!(report.errors.is_empty());
    assert!(
        report.warnings.iter().any(|w| w.contains("Tool definitions")),
        "Should warn about tools"
    );
    assert!(
        report.warnings.iter().any(|w| w.contains("Plugin configurations")),
        "Should warn about plugins"
    );
    assert!(
        report.warnings.iter().any(|w| w.contains("Custom commands")),
        "Should warn about commands"
    );
    assert!(
        report.warnings.iter().any(|w| w.contains("Skill definitions")),
        "Should warn about skills"
    );

    Ok(())
}

// ── Gate 4: Collision clean failure ─────────────────────────────

#[test]
fn gate4_collision_clean_failure() -> Result<()> {
    let tmp = TempDir::new()?;
    let openclaw_dir = tmp.path().join("openclaw");
    fs::create_dir_all(&openclaw_dir)?;

    let agent_dir = openclaw_dir.join("agents").join("main").join("agent");
    fs::create_dir_all(&agent_dir)?;
    create_models_json(&agent_dir, "anthropic", "sk-ant-test-key-12345678");

    let ws = tmp.path().join("ws");
    fs::create_dir_all(&ws)?;

    let config = make_openclaw_config(
        &format!(r#"{{ "id": "main", "workspace": "{}" }}"#, ws.display()),
        "",
        "",
        "",
    );

    let config_path = openclaw_dir.join("openclaw.json");
    fs::write(&config_path, config)?;

    let cp_dir = tmp.path().join("cp");
    let instances_dir = cp_dir.join("instances");
    fs::create_dir_all(&instances_dir)?;

    let registry = Registry::open(&cp_dir.join("registry.db"))?;

    // Pre-create instance named "main"
    registry.create_instance("pre-existing", "main", 18801, "/c.toml", None, None)?;

    let report = migrate::openclaw::run_openclaw_migration(
        &config_path,
        false,
        &cp_dir,
        &registry,
        &instances_dir,
    )?;

    assert!(!report.errors.is_empty(), "Should have collision errors");
    assert!(report.created.is_empty(), "Zero new instances on collision");

    // No orphan dirs
    let dirs: Vec<_> = fs::read_dir(&instances_dir)?
        .filter_map(|e| e.ok())
        .collect();
    assert!(dirs.is_empty(), "No FS orphans on collision");

    Ok(())
}

// ── Gate 5: Dry-run parity ──────────────────────────────────────

#[test]
fn gate5_dry_run_parity() -> Result<()> {
    let tmp = TempDir::new()?;

    // Run A: dry-run
    let (config_path_a, cp_dir_a, instances_dir_a) = {
        let openclaw_dir = tmp.path().join("oc-a");
        fs::create_dir_all(&openclaw_dir)?;
        let agent_dir = openclaw_dir.join("agents").join("solo").join("agent");
        fs::create_dir_all(&agent_dir)?;
        create_models_json(&agent_dir, "anthropic", "sk-ant-parity-key-12345678");
        let ws = tmp.path().join("ws-a");
        fs::create_dir_all(&ws)?;
        let config = make_openclaw_config(
            &format!(r#"{{ "id": "solo", "workspace": "{}" }}"#, ws.display()),
            "",
            "",
            "",
        );
        let config_path = openclaw_dir.join("openclaw.json");
        fs::write(&config_path, &config)?;
        let cp_dir = tmp.path().join("cp-a");
        let instances_dir = cp_dir.join("instances");
        fs::create_dir_all(&instances_dir)?;
        (config_path, cp_dir, instances_dir)
    };

    let registry_a = Registry::open(&cp_dir_a.join("registry.db"))?;
    let report_a = migrate::openclaw::run_openclaw_migration(
        &config_path_a,
        true,
        &cp_dir_a,
        &registry_a,
        &instances_dir_a,
    )?;

    // Run B: real (fresh DB)
    let (config_path_b, cp_dir_b, instances_dir_b) = {
        let openclaw_dir = tmp.path().join("oc-b");
        fs::create_dir_all(&openclaw_dir)?;
        let agent_dir = openclaw_dir.join("agents").join("solo").join("agent");
        fs::create_dir_all(&agent_dir)?;
        create_models_json(&agent_dir, "anthropic", "sk-ant-parity-key-12345678");
        let ws = tmp.path().join("ws-b");
        fs::create_dir_all(&ws)?;
        let config = make_openclaw_config(
            &format!(r#"{{ "id": "solo", "workspace": "{}" }}"#, ws.display()),
            "",
            "",
            "",
        );
        let config_path = openclaw_dir.join("openclaw.json");
        fs::write(&config_path, &config)?;
        let cp_dir = tmp.path().join("cp-b");
        let instances_dir = cp_dir.join("instances");
        fs::create_dir_all(&instances_dir)?;
        (config_path, cp_dir, instances_dir)
    };

    let registry_b = Registry::open(&cp_dir_b.join("registry.db"))?;
    let report_b = migrate::openclaw::run_openclaw_migration(
        &config_path_b,
        false,
        &cp_dir_b,
        &registry_b,
        &instances_dir_b,
    )?;

    // Compare: same agent IDs, ports, warnings. UUIDs differ (excluded).
    assert_eq!(report_a.created.len(), report_b.created.len());
    for (a, b) in report_a.created.iter().zip(report_b.created.iter()) {
        assert_eq!(a.agent_id, b.agent_id);
        assert_eq!(a.port, b.port);
        assert_eq!(a.channels, b.channels);
        // UUIDs differ -- that's expected
        assert_ne!(a.instance_id, b.instance_id);
    }
    assert_eq!(report_a.warnings.len(), report_b.warnings.len());

    Ok(())
}

// ── Gate 6: Idempotency block ───────────────────────────────────

#[test]
fn gate6_idempotency_block() -> Result<()> {
    let tmp = TempDir::new()?;
    let (config_path, cp_dir, instances_dir) = setup_two_agent_env(&tmp);

    let registry = Registry::open(&cp_dir.join("registry.db"))?;

    // First migration: success
    let report1 = migrate::openclaw::run_openclaw_migration(
        &config_path,
        false,
        &cp_dir,
        &registry,
        &instances_dir,
    )?;
    assert!(report1.errors.is_empty());
    assert_eq!(report1.created.len(), 2);

    let count_after_first = registry.list_instances()?.len();

    // Second migration: should fail with collision errors
    let report2 = migrate::openclaw::run_openclaw_migration(
        &config_path,
        false,
        &cp_dir,
        &registry,
        &instances_dir,
    )?;
    assert!(!report2.errors.is_empty(), "Second migration should have errors");
    assert!(report2.created.is_empty(), "No new instances on re-migration");

    // Instance count unchanged
    assert_eq!(registry.list_instances()?.len(), count_after_first);

    Ok(())
}

// ── Gate 7: Imported instances startable (static checks) ────────

#[test]
fn gate7_static_startability_checks() -> Result<()> {
    let tmp = TempDir::new()?;
    let (config_path, cp_dir, instances_dir) = setup_two_agent_env(&tmp);

    let registry = Registry::open(&cp_dir.join("registry.db"))?;

    let report = migrate::openclaw::run_openclaw_migration(
        &config_path,
        false,
        &cp_dir,
        &registry,
        &instances_dir,
    )?;

    assert!(report.errors.is_empty(), "Errors: {:?}", report.errors);

    for entry in &report.created {
        // a) Registry: instance exists and is stopped
        let inst = registry.get_instance(&entry.instance_id)?.unwrap();
        assert_eq!(inst.status, "stopped");

        // b) Config path is file with 0600 perms
        let config_toml_path = instances_dir
            .join(&entry.instance_id)
            .join("config.toml");
        assert!(config_toml_path.is_file(), "config.toml must exist");
        let perms = fs::metadata(&config_toml_path)?.permissions();
        assert_eq!(
            perms.mode() & 0o777,
            0o600,
            "Config must have 0600 permissions"
        );

        // c) Valid TOML with required gateway fields
        let content = fs::read_to_string(&config_toml_path)?;
        let parsed: toml::Value = toml::from_str(&content)?;
        let gw = &parsed["gateway"];
        assert!(gw["port"].as_integer().is_some(), "gateway.port must exist");
        assert!(gw["host"].as_str().is_some(), "gateway.host must exist");
        assert_eq!(
            gw["require_pairing"].as_bool(),
            Some(true),
            "gateway.require_pairing must be true"
        );

        // d) api_key present (our test setup provides it)
        assert!(
            parsed.get("api_key").is_some(),
            "api_key should be present"
        );

        // e) channels_config.telegram present (our test has telegram bindings)
        assert!(
            parsed.get("channels_config").is_some(),
            "channels_config should be present"
        );
    }

    eprintln!("NOTE: Runtime health gate (test 8) requires --ignored flag");
    Ok(())
}

// ── Gate 8: Runtime health gate ─────────────────────────────────

#[test]
#[ignore]
fn gate8_runtime_health_gate() {
    let zeroclaw_bin = std::env::var("ZEROCLAW_BIN")
        .unwrap_or_else(|_| panic!("ZEROCLAW_BIN required for runtime health gate"));

    let tmp = TempDir::new().unwrap();
    let (config_path, cp_dir, instances_dir) = setup_two_agent_env(&tmp);

    let registry = Registry::open(&cp_dir.join("registry.db")).unwrap();

    let report = migrate::openclaw::run_openclaw_migration(
        &config_path,
        false,
        &cp_dir,
        &registry,
        &instances_dir,
    )
    .unwrap();

    assert!(report.errors.is_empty());

    // Start each instance and verify /health
    for entry in &report.created {
        let instance_dir = instances_dir.join(&entry.instance_id);
        let _config_toml = instance_dir.join("config.toml");

        let mut child = std::process::Command::new(&zeroclaw_bin)
            .arg("daemon")
            .arg("--port")
            .arg(entry.port.to_string())
            .env("ZEROCLAW_HOME", instance_dir.to_str().unwrap())
            .spawn()
            .expect("Failed to start zeroclaw instance");

        // Poll /health for up to 10 seconds
        let start = std::time::Instant::now();
        let mut healthy = false;
        while start.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if let Ok(resp) =
                reqwest::blocking::get(format!("http://127.0.0.1:{}/health", entry.port))
            {
                if resp.status().is_success() {
                    healthy = true;
                    break;
                }
            }
        }

        child.kill().ok();
        child.wait().ok();

        assert!(
            healthy,
            "Instance {} (port {}) did not respond to /health within 10s",
            entry.agent_id, entry.port
        );
    }
}
