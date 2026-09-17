#![cfg(unix)]

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use serde_json::json;
use tempfile::TempDir;
use tokio::sync::{Mutex, MutexGuard};
use zeroclaw_api::tool::Tool;
use zeroclaw_config::platform::DockerRuntime;
use zeroclaw_config::schema::{Config, RuntimeKind};
use zeroclaw_runtime::security::{AutonomyLevel, SecurityPolicy, sandbox_posture};
use zeroclaw_runtime::tools::shell_tool_for_runtime;

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: this integration-test binary serializes its environment changes.
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }

    fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: this integration-test binary serializes its environment changes.
        unsafe { std::env::remove_var(key) };
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: this integration-test binary serializes its environment changes.
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

async fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.get_or_init(|| Mutex::new(())).lock().await
}

fn write_fake_docker(bin_dir: &Path) {
    std::fs::create_dir_all(bin_dir).expect("create fake Docker bin directory");
    let docker = bin_dir.join("docker");
    std::fs::write(
        &docker,
        "#!/bin/sh\nfor arg in \"$@\"; do\n  printf '%s\\n' \"$arg\"\ndone\n",
    )
    .expect("write fake Docker executable");
    let mut permissions = std::fs::metadata(&docker)
        .expect("read fake Docker metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(docker, permissions).expect("make fake Docker executable");
}

#[tokio::test(flavor = "current_thread")]
async fn config_loaded_docker_runtime_executes_one_docker_run_through_shell_tool() {
    let _env_lock = env_lock().await;
    let install = TempDir::new().expect("create isolated install");
    let bin_dir = install.path().join("bin");
    write_fake_docker(&bin_dir);

    std::fs::write(
        install.path().join("config.toml"),
        r#"
schema_version = 3

[runtime]
kind = "docker"

[runtime.docker]
image = "library/alpine:3.23"
network = "bridge"
mount_workspace = false
read_only_rootfs = true

[risk_profiles.default]
level = "full"
sandbox_backend = "docker"
"#,
    )
    .expect("write config fixture");

    let _config_dir = EnvGuard::set("ZEROCLAW_CONFIG_DIR", install.path());
    let _data_dir = EnvGuard::remove("ZEROCLAW_DATA_DIR");
    let _legacy_workspace = EnvGuard::remove("ZEROCLAW_WORKSPACE");

    let config = Config::load_or_init().await.expect("load config fixture");
    assert_eq!(config.runtime.kind, RuntimeKind::Docker);

    let risk_profile = config
        .risk_profiles
        .get("default")
        .expect("default risk profile");
    let posture = sandbox_posture(
        &risk_profile.sandbox_config(),
        config.runtime.kind,
        Some(install.path()),
        &zeroclaw_runtime::security::SandboxExtraRoots::default(),
    );
    assert_eq!(
        posture.active_backend, "docker-runtime",
        "Docker runtime must own the only Docker execution layer"
    );
    assert!(
        !posture.fallback,
        "runtime-owned containment must not be reported as a lost fallback"
    );

    let security = Arc::new(SecurityPolicy {
        autonomy: AutonomyLevel::Full,
        workspace_dir: install.path().to_path_buf(),
        allowed_commands: vec!["*".into()],
        block_high_risk_commands: false,
        ..SecurityPolicy::default()
    });
    let runtime = Arc::new(DockerRuntime::new(config.runtime.docker.clone()));
    let tool = shell_tool_for_runtime(security, runtime, risk_profile, &config).with_tui_env(Some(
        HashMap::from([("PATH".to_string(), bin_dir.to_string_lossy().into_owned())]),
    ));

    let result = tool
        .execute(json!({"command": "echo issue-9231"}))
        .await
        .expect("execute assembled shell tool");
    assert!(
        result.success,
        "fake Docker invocation should succeed: {:?}",
        result.error
    );

    let args = result.output.lines().collect::<Vec<_>>();
    assert_eq!(
        args.iter().filter(|arg| **arg == "run").count(),
        1,
        "assembled command nested Docker: {args:?}"
    );
    assert!(
        !args.contains(&"docker"),
        "Docker sandbox inserted a second Docker program: {args:?}"
    );
    assert!(
        !args.contains(&"alpine:latest"),
        "Docker sandbox image shadowed runtime config: {args:?}"
    );
    assert!(args.contains(&"library/alpine:3.23"), "{args:?}");
    assert!(args.contains(&"bridge"), "{args:?}");
    assert!(args.contains(&"--read-only"), "{args:?}");
    assert!(args.contains(&"echo issue-9231"), "{args:?}");
}
