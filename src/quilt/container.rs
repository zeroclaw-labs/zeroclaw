use std::collections::HashMap;
use std::sync::Mutex;

use tracing::{debug, info, warn};

use super::client::{
    QuiltClient, QuiltContainerState, QuiltContainerStatus, QuiltCreateParams, QuiltExecParams,
};
use super::config::SandboxQuiltConfig;

// ── Constants ───────────────────────────────────────────────────────

/// Containers used within this window are considered "hot" and will not be
/// replaced even if the config hash has changed.
const HOT_CONTAINER_WINDOW_MS: i64 = 5 * 60 * 1000; // 5 minutes

/// Default working directory inside sandbox containers.
const DEFAULT_WORKDIR: &str = "/workspace";

/// Default container image for sandboxes.
const DEFAULT_IMAGE: &str = "ubuntu:22.04";

// ── Label keys ──────────────────────────────────────────────────────

const LABEL_SANDBOX: &str = "aria.sandbox";
const LABEL_SESSION_KEY: &str = "aria.session_key";
const LABEL_CREATED_AT_MS: &str = "aria.created_at_ms";
const LABEL_CONFIG_HASH: &str = "aria.config_hash";

// ── SandboxContext ──────────────────────────────────────────────────

/// Returned after successfully ensuring a sandbox container is available.
#[derive(Debug, Clone)]
pub struct SandboxContext {
    pub container_id: String,
    pub container_name: String,
    pub container_workdir: String,
    pub ip_address: Option<String>,
}

// ── Container registry ──────────────────────────────────────────────

/// In-memory registry mapping session keys to container IDs.
/// Avoids redundant API lookups when a session already has a container.
static CONTAINER_REGISTRY: std::sync::OnceLock<Mutex<HashMap<String, String>>> =
    std::sync::OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, String>> {
    CONTAINER_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a container for a session.
pub fn register_container(session_key: &str, container_id: &str) {
    let mut reg = registry().lock().expect("container registry lock poisoned");
    reg.insert(session_key.to_string(), container_id.to_string());
}

/// Look up a container ID for a session.
pub fn lookup_container(session_key: &str) -> Option<String> {
    let reg = registry().lock().expect("container registry lock poisoned");
    reg.get(session_key).cloned()
}

/// Remove a container from the registry.
pub fn deregister_container(session_key: &str) {
    let mut reg = registry().lock().expect("container registry lock poisoned");
    reg.remove(session_key);
}

/// Clear the entire registry (for tests).
#[cfg(test)]
pub fn clear_registry() {
    let mut reg = registry().lock().expect("container registry lock poisoned");
    reg.clear();
}

// ── Core logic ──────────────────────────────────────────────────────

/// Returns the current time in epoch milliseconds.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Determine whether a container is "hot" (recently used).
fn is_hot_container(status: &QuiltContainerStatus) -> bool {
    if let Some(started) = status.started_at_ms {
        let elapsed = now_ms() - started;
        return elapsed < HOT_CONTAINER_WINDOW_MS;
    }
    false
}

/// Check whether the container's config hash matches the desired one.
fn config_hash_matches(status: &QuiltContainerStatus, desired_hash: &str) -> bool {
    status
        .labels
        .as_ref()
        .and_then(|l| l.get(LABEL_CONFIG_HASH))
        .map_or(false, |h| h == desired_hash)
}

/// Build labels for a new sandbox container.
fn build_labels(session_key: &str, config_hash: &str) -> HashMap<String, String> {
    let mut labels = HashMap::new();
    labels.insert(LABEL_SANDBOX.into(), "true".into());
    labels.insert(LABEL_SESSION_KEY.into(), session_key.into());
    labels.insert(LABEL_CREATED_AT_MS.into(), now_ms().to_string());
    labels.insert(LABEL_CONFIG_HASH.into(), config_hash.into());
    labels
}

/// Generate a container name from a session key.
fn container_name(session_key: &str) -> String {
    // Sanitize session key for use as a container name: keep alphanumeric + hyphens
    let sanitized: String = session_key
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    format!("sandbox-{sanitized}")
}

/// Ensure a sandbox container is available for the given session.
///
/// This function:
/// 1. Checks the in-memory registry for an existing container ID
/// 2. Validates the container is still running and config matches
/// 3. Skips replacement if the container is "hot" (used within 5 minutes)
/// 4. Creates a new container if needed
/// 5. Runs the optional `setup_command` after creation
pub async fn ensure_sandbox_container(
    client: &QuiltClient,
    config: &SandboxQuiltConfig,
    session_key: &str,
) -> Result<SandboxContext, anyhow::Error> {
    let desired_hash = config.config_hash();
    let name = container_name(session_key);

    // 1. Check registry for existing container
    if let Some(existing_id) = lookup_container(session_key) {
        debug!(session_key, container_id = %existing_id, "Found registered container");

        match client.get_container(&existing_id).await {
            Ok(status) => {
                if status.state == QuiltContainerState::Running {
                    // Check config hash
                    if config_hash_matches(&status, &desired_hash) {
                        debug!("Existing container matches config, reusing");
                        return Ok(SandboxContext {
                            container_id: status.id,
                            container_name: status.name,
                            container_workdir: DEFAULT_WORKDIR.into(),
                            ip_address: status.ip_address,
                        });
                    }

                    // Config changed but container is hot -- keep it
                    if is_hot_container(&status) {
                        info!(
                            "Config changed but container is hot (used within 5 min), keeping it"
                        );
                        return Ok(SandboxContext {
                            container_id: status.id,
                            container_name: status.name,
                            container_workdir: DEFAULT_WORKDIR.into(),
                            ip_address: status.ip_address,
                        });
                    }

                    // Config changed and container is cold -- replace it
                    info!("Config changed and container is cold, replacing");
                    let _ = client.stop_container(&existing_id).await;
                    let _ = client.delete_container(&existing_id).await;
                    deregister_container(session_key);
                } else {
                    // Container exists but not running -- clean up
                    debug!(state = %status.state, "Container not running, cleaning up");
                    let _ = client.delete_container(&existing_id).await;
                    deregister_container(session_key);
                }
            }
            Err(e) => {
                // Container not found or API error -- deregister and create new
                warn!(error = %e, "Could not fetch registered container, will create new");
                deregister_container(session_key);
            }
        }
    }

    // 2. Try to find by name (in case registry was lost but container exists)
    match client.get_container_by_name(&name).await {
        Ok(status) if status.state == QuiltContainerState::Running => {
            if config_hash_matches(&status, &desired_hash) || is_hot_container(&status) {
                debug!("Found running container by name, reusing");
                register_container(session_key, &status.id);
                return Ok(SandboxContext {
                    container_id: status.id,
                    container_name: status.name,
                    container_workdir: DEFAULT_WORKDIR.into(),
                    ip_address: status.ip_address,
                });
            }
            // Stale container with wrong config -- replace
            let _ = client.stop_container(&status.id).await;
            let _ = client.delete_container(&status.id).await;
        }
        Ok(status) => {
            // Exists but not running
            let _ = client.delete_container(&status.id).await;
        }
        Err(_) => {
            // Not found, will create
        }
    }

    // 3. Create a new container
    info!(session_key, name = %name, "Creating new sandbox container");

    let labels = build_labels(session_key, &desired_hash);

    let create_params = QuiltCreateParams {
        name: name.clone(),
        image: DEFAULT_IMAGE.into(),
        command: Some(vec![
            "bash".into(),
            "-c".into(),
            "sleep infinity".into(),
        ]),
        environment: HashMap::new(),
        volumes: vec![],
        ports: vec![],
        memory_limit_mb: Some(config.memory_limit_mb),
        cpu_limit_percent: Some(config.cpu_limit_percent),
        labels,
        network: None,
        restart_policy: None,
    };

    let result = client.create_container(create_params).await?;
    let container_id = result.container_id.clone();

    // Start the container
    client.start_container(&container_id).await?;

    // Register in the in-memory map
    register_container(session_key, &container_id);

    info!(container_id = %container_id, "Sandbox container created and started");

    // 4. Run setup command if configured
    if let Some(ref setup_cmd) = config.setup_command {
        info!(cmd = %setup_cmd, "Running setup command");
        let exec_params = QuiltExecParams {
            command: vec!["bash".into(), "-c".into(), setup_cmd.clone()],
            timeout_ms: Some(120_000), // 2 minute timeout for setup
            working_dir: Some(DEFAULT_WORKDIR.into()),
            environment: None,
        };

        match client.exec(&container_id, exec_params).await {
            Ok(exec_result) => {
                if exec_result.exit_code != 0 {
                    warn!(
                        exit_code = exec_result.exit_code,
                        stderr = %exec_result.stderr,
                        "Setup command exited with non-zero code"
                    );
                } else {
                    debug!("Setup command completed successfully");
                }
            }
            Err(e) => {
                warn!(error = %e, "Setup command failed");
            }
        }
    }

    Ok(SandboxContext {
        container_id: result.container_id,
        container_name: result.name,
        container_workdir: DEFAULT_WORKDIR.into(),
        ip_address: result.ip_address,
    })
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── container_name ──────────────────────────────────────────

    #[test]
    fn container_name_simple() {
        assert_eq!(container_name("abc-123"), "sandbox-abc-123");
    }

    #[test]
    fn container_name_sanitizes_special_chars() {
        assert_eq!(container_name("user@host:session"), "sandbox-user-host-session");
    }

    #[test]
    fn container_name_preserves_hyphens() {
        assert_eq!(container_name("my-session-key"), "sandbox-my-session-key");
    }

    #[test]
    fn container_name_sanitizes_spaces() {
        assert_eq!(container_name("has spaces"), "sandbox-has-spaces");
    }

    // ── build_labels ────────────────────────────────────────────

    #[test]
    fn build_labels_contains_required_keys() {
        let labels = build_labels("sess-1", "hash123");
        assert_eq!(labels.get(LABEL_SANDBOX).unwrap(), "true");
        assert_eq!(labels.get(LABEL_SESSION_KEY).unwrap(), "sess-1");
        assert_eq!(labels.get(LABEL_CONFIG_HASH).unwrap(), "hash123");
        assert!(labels.get(LABEL_CREATED_AT_MS).is_some());
        // created_at_ms should be a valid number
        let ts: i64 = labels.get(LABEL_CREATED_AT_MS).unwrap().parse().unwrap();
        assert!(ts > 0);
    }

    // ── config_hash_matches ─────────────────────────────────────

    #[test]
    fn config_hash_matches_when_equal() {
        let status = QuiltContainerStatus {
            id: "ctr-1".into(),
            tenant_id: None,
            name: "sandbox-test".into(),
            state: QuiltContainerState::Running,
            pid: None,
            exit_code: None,
            ip_address: None,
            memory_limit_mb: None,
            cpu_limit_percent: None,
            labels: Some(HashMap::from([
                (LABEL_CONFIG_HASH.into(), "abc123".into()),
            ])),
            started_at_ms: None,
            exited_at_ms: None,
        };
        assert!(config_hash_matches(&status, "abc123"));
        assert!(!config_hash_matches(&status, "different"));
    }

    #[test]
    fn config_hash_no_match_when_no_labels() {
        let status = QuiltContainerStatus {
            id: "ctr-1".into(),
            tenant_id: None,
            name: "sandbox-test".into(),
            state: QuiltContainerState::Running,
            pid: None,
            exit_code: None,
            ip_address: None,
            memory_limit_mb: None,
            cpu_limit_percent: None,
            labels: None,
            started_at_ms: None,
            exited_at_ms: None,
        };
        assert!(!config_hash_matches(&status, "any"));
    }

    #[test]
    fn config_hash_no_match_when_label_missing() {
        let status = QuiltContainerStatus {
            id: "ctr-1".into(),
            tenant_id: None,
            name: "sandbox-test".into(),
            state: QuiltContainerState::Running,
            pid: None,
            exit_code: None,
            ip_address: None,
            memory_limit_mb: None,
            cpu_limit_percent: None,
            labels: Some(HashMap::from([
                ("other.label".into(), "value".into()),
            ])),
            started_at_ms: None,
            exited_at_ms: None,
        };
        assert!(!config_hash_matches(&status, "any"));
    }

    // ── is_hot_container ────────────────────────────────────────

    #[test]
    fn hot_container_recently_started() {
        let status = QuiltContainerStatus {
            id: "ctr-1".into(),
            tenant_id: None,
            name: "sandbox-test".into(),
            state: QuiltContainerState::Running,
            pid: None,
            exit_code: None,
            ip_address: None,
            memory_limit_mb: None,
            cpu_limit_percent: None,
            labels: None,
            started_at_ms: Some(now_ms() - 1000), // 1 second ago
            exited_at_ms: None,
        };
        assert!(is_hot_container(&status));
    }

    #[test]
    fn cold_container_old_start() {
        let status = QuiltContainerStatus {
            id: "ctr-1".into(),
            tenant_id: None,
            name: "sandbox-test".into(),
            state: QuiltContainerState::Running,
            pid: None,
            exit_code: None,
            ip_address: None,
            memory_limit_mb: None,
            cpu_limit_percent: None,
            labels: None,
            started_at_ms: Some(now_ms() - HOT_CONTAINER_WINDOW_MS - 1000), // 6 minutes ago
            exited_at_ms: None,
        };
        assert!(!is_hot_container(&status));
    }

    #[test]
    fn cold_container_no_start_time() {
        let status = QuiltContainerStatus {
            id: "ctr-1".into(),
            tenant_id: None,
            name: "sandbox-test".into(),
            state: QuiltContainerState::Running,
            pid: None,
            exit_code: None,
            ip_address: None,
            memory_limit_mb: None,
            cpu_limit_percent: None,
            labels: None,
            started_at_ms: None,
            exited_at_ms: None,
        };
        assert!(!is_hot_container(&status));
    }

    // ── Registry ────────────────────────────────────────────────

    #[test]
    fn registry_crud() {
        clear_registry();

        assert!(lookup_container("test-session").is_none());

        register_container("test-session", "ctr-abc");
        assert_eq!(lookup_container("test-session").unwrap(), "ctr-abc");

        // Overwrite
        register_container("test-session", "ctr-def");
        assert_eq!(lookup_container("test-session").unwrap(), "ctr-def");

        deregister_container("test-session");
        assert!(lookup_container("test-session").is_none());

        clear_registry();
    }

    #[test]
    fn registry_multiple_sessions() {
        clear_registry();

        register_container("sess-1", "ctr-1");
        register_container("sess-2", "ctr-2");
        register_container("sess-3", "ctr-3");

        assert_eq!(lookup_container("sess-1").unwrap(), "ctr-1");
        assert_eq!(lookup_container("sess-2").unwrap(), "ctr-2");
        assert_eq!(lookup_container("sess-3").unwrap(), "ctr-3");

        deregister_container("sess-2");
        assert!(lookup_container("sess-2").is_none());
        assert_eq!(lookup_container("sess-1").unwrap(), "ctr-1");

        clear_registry();
    }

    // ── SandboxContext ──────────────────────────────────────────

    #[test]
    fn sandbox_context_debug() {
        let ctx = SandboxContext {
            container_id: "ctr-123".into(),
            container_name: "sandbox-test".into(),
            container_workdir: "/workspace".into(),
            ip_address: Some("10.0.0.5".into()),
        };
        let debug = format!("{ctx:?}");
        assert!(debug.contains("ctr-123"));
        assert!(debug.contains("sandbox-test"));
        assert!(debug.contains("/workspace"));
    }

    #[test]
    fn sandbox_context_clone() {
        let ctx = SandboxContext {
            container_id: "ctr-clone".into(),
            container_name: "sandbox-clone".into(),
            container_workdir: "/workspace".into(),
            ip_address: None,
        };
        let cloned = ctx.clone();
        assert_eq!(cloned.container_id, ctx.container_id);
        assert_eq!(cloned.container_name, ctx.container_name);
        assert!(cloned.ip_address.is_none());
    }

    // ── Constants ───────────────────────────────────────────────

    #[test]
    fn hot_container_window_is_five_minutes() {
        assert_eq!(HOT_CONTAINER_WINDOW_MS, 300_000);
    }

    #[test]
    fn default_workdir_is_workspace() {
        assert_eq!(DEFAULT_WORKDIR, "/workspace");
    }

    #[test]
    fn default_image_is_ubuntu() {
        assert_eq!(DEFAULT_IMAGE, "ubuntu:22.04");
    }
}
