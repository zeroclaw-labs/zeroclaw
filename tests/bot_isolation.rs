//! Integration tests for bot workspace isolation (Phase 2).
//!
//! Run with: cargo test --test bot_isolation

use tempfile::TempDir;
use zeroclaw::config::schema::{BotConfig, BotRateLimiter, Config};
use zeroclaw::memory::{Memory, MemoryCategory, sqlite::SqliteMemory};

// ── Helpers ────────────────────────────────────────────────────

fn base_config(workspace: &std::path::Path) -> Config {
    Config {
        workspace_dir: workspace.to_path_buf(),
        ..Config::default()
    }
}

fn bot(port: u16) -> BotConfig {
    BotConfig {
        name: None,
        workspace_dir: None,
        identity: None,
        soul: None,
        port,
        provider: None,
        model: None,
        api_key: None,
        temperature: None,
        system_prompt: None,
        channels: None,
        memory: None,
        max_memory_mb: None,
        max_concurrent_requests: None,
        max_tokens_per_minute: None,
    }
}

// ── Test 1: Two bots with different workspaces cannot read each other's memory

#[tokio::test]
async fn bot_memory_isolation() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path();

    let dir_a = base.join("bots").join("alpha");
    let dir_b = base.join("bots").join("beta");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();

    let mem_a = SqliteMemory::new(&dir_a).expect("init alpha memory");
    let mem_b = SqliteMemory::new(&dir_b).expect("init beta memory");

    mem_a
        .store("secret", "alpha-only-data", MemoryCategory::Core, None)
        .await
        .expect("alpha store");

    mem_b
        .store("secret", "beta-only-data", MemoryCategory::Core, None)
        .await
        .expect("beta store");

    let recalled_a = mem_a
        .recall("secret", 100, None)
        .await
        .expect("alpha recall");
    let recalled_b = mem_b
        .recall("secret", 100, None)
        .await
        .expect("beta recall");

    let a_contents: Vec<&str> = recalled_a.iter().map(|e| e.content.as_str()).collect();
    let b_contents: Vec<&str> = recalled_b.iter().map(|e| e.content.as_str()).collect();

    assert!(
        a_contents.contains(&"alpha-only-data"),
        "alpha should see its own data"
    );
    assert!(
        !a_contents.contains(&"beta-only-data"),
        "alpha must not see beta data"
    );
    assert!(
        b_contents.contains(&"beta-only-data"),
        "beta should see its own data"
    );
    assert!(
        !b_contents.contains(&"alpha-only-data"),
        "beta must not see alpha data"
    );
}

// ── Test 2: Bot config resolution correctly overlays fields

#[test]
fn bot_config_overlay() {
    let tmp = TempDir::new().unwrap();
    let mut config = base_config(tmp.path());

    let mut bot_alpha = bot(9001);
    bot_alpha.provider = Some("anthropic".into());
    bot_alpha.model = Some("claude-sonnet-4.6".into());
    bot_alpha.temperature = Some(0.3);
    bot_alpha.max_memory_mb = Some(1024);
    bot_alpha.max_concurrent_requests = Some(5);
    bot_alpha.max_tokens_per_minute = Some(50_000);

    config.bots.insert("alpha".into(), bot_alpha);

    let resolved = config.resolve_bot_config("alpha");

    assert_eq!(
        resolved.default_provider.as_deref(),
        Some("anthropic"),
        "provider overlay"
    );
    assert_eq!(
        resolved.default_model.as_deref(),
        Some("claude-sonnet-4.6"),
        "model overlay"
    );
    assert!(
        (resolved.default_temperature - 0.3).abs() < f64::EPSILON,
        "temperature overlay"
    );
    assert_eq!(resolved.gateway.port, 9001, "port overlay");
    assert_eq!(resolved.max_memory_mb, 1024, "max_memory_mb overlay");
    assert_eq!(
        resolved.max_concurrent_requests, 5,
        "max_concurrent_requests overlay"
    );
    assert_eq!(
        resolved.max_tokens_per_minute, 50_000,
        "max_tokens_per_minute overlay"
    );
    assert!(
        resolved.bots.is_empty(),
        "resolved config should have empty bots map"
    );
}

#[test]
fn bot_config_overlay_uses_defaults_when_unset() {
    let tmp = TempDir::new().unwrap();
    let mut config = base_config(tmp.path());
    config.bots.insert("minimal".into(), bot(9002));

    let resolved = config.resolve_bot_config("minimal");

    assert_eq!(resolved.max_memory_mb, 512, "default max_memory_mb");
    assert_eq!(
        resolved.max_concurrent_requests, 10,
        "default max_concurrent_requests"
    );
    assert_eq!(
        resolved.max_tokens_per_minute, 100_000,
        "default max_tokens_per_minute"
    );
}

// ── Test 3: Port validation catches duplicate ports

#[test]
fn bot_port_duplicate_detected() {
    let tmp = TempDir::new().unwrap();
    let mut config = base_config(tmp.path());

    config.bots.insert("bot_a".into(), bot(8080));
    config.bots.insert("bot_b".into(), bot(8080));

    let result = config.validate_bot_ports();
    assert!(result.is_err(), "duplicate ports must be rejected");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("port conflict"),
        "error should mention port conflict: {msg}"
    );
}

#[test]
fn bot_port_unique_passes() {
    let tmp = TempDir::new().unwrap();
    let mut config = base_config(tmp.path());

    config.bots.insert("bot_a".into(), bot(8080));
    config.bots.insert("bot_b".into(), bot(8081));

    assert!(
        config.validate_bot_ports().is_ok(),
        "unique ports should pass validation"
    );
}

// ── Test 4: Default workspace_dir is <global>/bots/<bot_name>/

#[test]
fn bot_default_workspace_dir() {
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let mut config = base_config(&workspace);
    config.bots.insert("mybot".into(), bot(9003));

    let resolved = config.resolve_bot_config("mybot");

    let expected = tmp.path().join("bots").join("mybot");
    assert_eq!(
        resolved.workspace_dir, expected,
        "default bot workspace should be <parent>/bots/<bot_name>"
    );
}

#[test]
fn bot_custom_workspace_dir_overrides_default() {
    let tmp = TempDir::new().unwrap();
    let custom_dir = tmp.path().join("custom-bot-workspace");

    let mut config = base_config(tmp.path());
    let mut b = bot(9004);
    b.workspace_dir = Some(custom_dir.clone());
    config.bots.insert("custom".into(), b);

    let resolved = config.resolve_bot_config("custom");
    assert_eq!(
        resolved.workspace_dir, custom_dir,
        "custom workspace_dir should be used"
    );
}

// ── Test 5: BotRateLimiter basic behavior

#[test]
fn bot_rate_limiter_concurrency() {
    let limiter = BotRateLimiter::new(100_000, 2);

    assert!(limiter.try_acquire(), "first acquire");
    assert!(limiter.try_acquire(), "second acquire");
    assert!(
        !limiter.try_acquire(),
        "third acquire should fail at limit=2"
    );

    limiter.release();
    assert!(
        limiter.try_acquire(),
        "acquire after release should succeed"
    );
}

#[test]
fn bot_rate_limiter_token_tracking() {
    let limiter = BotRateLimiter::new(1000, 10);

    assert!(
        limiter.record_tokens(500),
        "500 of 1000 should be within budget"
    );
    assert_eq!(limiter.tokens_remaining(), 500);
    assert!(
        limiter.record_tokens(400),
        "900 of 1000 should be within budget"
    );
    assert_eq!(limiter.tokens_remaining(), 100);
    assert!(!limiter.record_tokens(200), "1100 exceeds 1000 budget");
}

#[test]
fn bot_rate_limiter_from_config() {
    let config = Config {
        max_tokens_per_minute: 50_000,
        max_concurrent_requests: 3,
        ..Config::default()
    };

    let limiter = BotRateLimiter::from_config(&config);

    assert!(limiter.try_acquire());
    assert!(limiter.try_acquire());
    assert!(limiter.try_acquire());
    assert!(
        !limiter.try_acquire(),
        "should respect config max_concurrent=3"
    );
}

// ── Test 6: Nonexistent bot returns clone of base config

#[test]
fn bot_resolve_nonexistent_returns_base() {
    let tmp = TempDir::new().unwrap();
    let config = base_config(tmp.path());

    let resolved = config.resolve_bot_config("does_not_exist");
    assert_eq!(resolved.workspace_dir, config.workspace_dir);
    assert_eq!(resolved.default_provider, config.default_provider);
}
