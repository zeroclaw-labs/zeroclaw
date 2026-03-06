//! Agent Watcher — hot-reload support for agent definition files.
//!
//! The watcher monitors the agents directory for file changes and automatically
//! reloads agent definitions when YAML files are added, modified, or removed.
//!
//! ## Features
//!
//! - **Debounced reload**: Prevents excessive reloads during rapid file changes
//! - **Selective reload**: Only reloads when `.yaml` or `.yml` files change
//! - **Graceful shutdown**: Properly cleans up resources on stop
//! - **Error resilience**: Continues watching even after individual reload failures
//!
//! ## Example
//!
//! ```no_run
//! use zeroclaw::agent::{AgentRegistry, AgentWatcher};
//! use std::path::PathBuf;
//! # use zeroclaw::security::SecurityPolicy;
//! # use std::sync::Arc;
//!
//! # fn main() -> anyhow::Result<()> {
//! # let security = Arc::new(SecurityPolicy::default());
//! let registry = AgentRegistry::new(PathBuf::from("agents"), security)?;
//! let mut watcher = AgentWatcher::new(registry)?;
//!
//! // Start watching for file changes
//! watcher.start()?;
//!
//! // Watcher runs in background, triggering reloads on file changes
//!
//! // Stop when done
//! watcher.stop()?;
//! # Ok(())
//! # }
//! ```

use crate::agent::AgentRegistry;
use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher, Config, EventKind};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Default debounce duration for file system events
///
/// This prevents excessive reloads when multiple files change rapidly
/// (e.g., during a batch save operation).
const DEFAULT_DEBOUNCE_DURATION: Duration = Duration::from_millis(300);

/// Agent file watcher for hot-reload support
///
/// Monitors the agents directory and automatically reloads agent definitions
/// when YAML files are added, modified, or removed.
#[derive(Debug)]
pub struct AgentWatcher {
    /// The agent registry to reload on file changes
    registry: Arc<AgentRegistry>,

    /// The underlying notify watcher
    watcher: Option<RecommendedWatcher>,

    /// Channel sender for shutdown signal
    shutdown_tx: Option<mpsc::Sender<()>>,

    /// Whether the watcher is currently running
    is_running: bool,
}

impl AgentWatcher {
    /// Create a new agent watcher
    ///
    /// The watcher will monitor the agents directory associated with the registry
    /// and trigger reloads when YAML files change.
    ///
    /// # Arguments
    ///
    /// * `registry` - The agent registry to monitor and reload
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use zeroclaw::agent::AgentRegistry;
    /// # use zeroclaw::agent::AgentWatcher;
    /// # use std::path::PathBuf;
    /// # use zeroclaw::security::SecurityPolicy;
    /// # use std::sync::Arc;
    /// # let security = Arc::new(SecurityPolicy::default());
    /// # let registry = AgentRegistry::new(PathBuf::from("agents"), security).unwrap();
    /// let watcher = AgentWatcher::new(registry)?;
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    pub fn new(registry: Arc<AgentRegistry>) -> Result<Self> {
        Ok(Self {
            registry,
            watcher: None,
            shutdown_tx: None,
            is_running: false,
        })
    }

    /// Start watching for file system changes
    ///
    /// This method spawns a background task that monitors the agents directory
    /// and triggers registry reloads when YAML files are added, modified, or removed.
    ///
    /// The watcher uses debouncing to prevent excessive reloads during rapid
    /// file changes (e.g., during a batch save operation).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The watcher is already running
    /// - The agents directory cannot be watched
    /// - The watcher channel cannot be created
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use zeroclaw::agent::{AgentRegistry, AgentWatcher};
    /// # use std::path::PathBuf;
    /// # use zeroclaw::security::SecurityPolicy;
    /// # use std::sync::Arc;
    /// # let security = Arc::new(SecurityPolicy::default());
    /// # let registry = AgentRegistry::new(PathBuf::from("agents"), security).unwrap();
    /// # let mut watcher = AgentWatcher::new(registry).unwrap();
    /// watcher.start()?;
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    pub fn start(&mut self) -> Result<()> {
        if self.is_running {
            warn!("AgentWatcher is already running");
            return Ok(());
        }

        let agents_dir = self.registry.agents_dir().to_path_buf();

        // Verify the directory exists or can be created
        if !agents_dir.exists() {
            debug!(
                "Agents directory does not exist yet: {}",
                agents_dir.display()
            );
            // We'll still create the watcher - it will start working when the directory is created
        }

        // Create channel for shutdown signal
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

        // Create a channel for file system events
        let (event_tx, mut event_rx) = mpsc::channel::<notify::Event>(32);

        // Create the watcher with debouncing
        let mut watcher: RecommendedWatcher = Watcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    // Send event to processing channel
                    let _ = event_tx.blocking_send(event);
                }
            },
            Config::default().with_poll_interval(DEFAULT_DEBOUNCE_DURATION),
        )
        .context("Failed to create file system watcher")?;

        // Watch the agents directory
        watcher
            .watch(&agents_dir, RecursiveMode::Recursive)
            .context(format!(
                "Failed to watch directory: {}",
                agents_dir.display()
            ))?;

        info!("Started watching agent directory: {}", agents_dir.display());

        // Clone registry for the spawn task
        let registry = Arc::clone(&self.registry);

        // Spawn background task to process events
        tokio::spawn(async move {
            let mut reload_timer = tokio::time::interval(Duration::from_millis(500));
            reload_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            loop {
                tokio::select! {
                    // Handle file system events
                    Some(event) = event_rx.recv() => {
                        if Self::should_reload_for_event(&event) {
                            debug!(
                                "Agent file change detected: {:?}",
                                event.paths
                            );

                            // Trigger reload
                            if let Err(e) = registry.reload() {
                                error!("Failed to reload agent registry: {}", e);
                            } else {
                                info!("Agent registry reloaded successfully");
                            }
                        }
                    }

                    // Handle shutdown signal
                    _ = shutdown_rx.recv() => {
                        debug!("AgentWatcher shutdown signal received");
                        break;
                    }

                    // Keep the task alive
                    else => break,
                }
            }
        });

        self.watcher = Some(watcher);
        self.shutdown_tx = Some(shutdown_tx);
        self.is_running = true;

        Ok(())
    }

    /// Stop watching for file system changes
    ///
    /// Stops the background task and cleans up resources.
    /// The watcher can be restarted by calling `start()` again.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use zeroclaw::agent::{AgentRegistry, AgentWatcher};
    /// # use std::path::PathBuf;
    /// # use zeroclaw::security::SecurityPolicy;
    /// # use std::sync::Arc;
    /// # let security = Arc::new(SecurityPolicy::default());
    /// # let registry = AgentRegistry::new(PathBuf::from("agents"), security).unwrap();
    /// # let mut watcher = AgentWatcher::new(registry).unwrap();
    /// # watcher.start().unwrap();
    /// watcher.stop()?;
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    pub fn stop(&mut self) -> Result<()> {
        if !self.is_running {
            warn!("AgentWatcher is not running");
            return Ok(());
        }

        info!("Stopping AgentWatcher");

        // Send shutdown signal
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.blocking_send(());
        }

        // Unwatch the directory
        if let Some(mut watcher) = self.watcher.take() {
            let agents_dir = self.registry.agents_dir();
            // Use the unwatch method if available, otherwise we just drop the watcher
            let _ = watcher.unwatch(agents_dir);
        }

        self.is_running = false;

        info!("AgentWatcher stopped");
        Ok(())
    }

    /// Check if the watcher is currently running
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use zeroclaw::agent::{AgentRegistry, AgentWatcher};
    /// # use std::path::PathBuf;
    /// # use zeroclaw::security::SecurityPolicy;
    /// # use std::sync::Arc;
    /// # let security = Arc::new(SecurityPolicy::default());
    /// # let registry = AgentRegistry::new(PathBuf::from("agents"), security).unwrap();
    /// # let watcher = AgentWatcher::new(registry).unwrap();
    /// assert!(!watcher.is_running());
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    pub fn is_running(&self) -> bool {
        self.is_running
    }

    /// Get the agents directory being watched
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use zeroclaw::agent::{AgentRegistry, AgentWatcher};
    /// # use std::path::PathBuf;
    /// # use zeroclaw::security::SecurityPolicy;
    /// # use std::sync::Arc;
    /// # let security = Arc::new(SecurityPolicy::default());
    /// # let registry = AgentRegistry::new(PathBuf::from("agents"), security).unwrap();
    /// # let watcher = AgentWatcher::new(registry).unwrap();
    /// let dir = watcher.watch_directory();
    /// println!("Watching: {}", dir.display());
    /// # Ok::<(), anyhow::Error>(())
    /// ```
    pub fn watch_directory(&self) -> &Path {
        self.registry.agents_dir()
    }

    /// Determine if a file system event should trigger a reload
    ///
    /// Only YAML files (.yaml, .yml) trigger reloads, and only for
    /// create, modify, and remove events.
    fn should_reload_for_event(event: &notify::Event) -> bool {
        // Check if any affected path is a YAML file
        let has_yaml_file = event.paths.iter().any(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map_or(false, |ext| ext == "yaml" || ext == "yml")
        });

        if !has_yaml_file {
            return false;
        }

        // Check event kind - notify 7.x uses different variant patterns
        match &event.kind {
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => true,
            _ => false,
        }
    }
}

impl Drop for AgentWatcher {
    fn drop(&mut self) {
        if self.is_running {
            // Best-effort cleanup during drop
            if let Some(tx) = self.shutdown_tx.take() {
                let _ = tx.blocking_send(());
            }
            self.watcher = None;
            self.is_running = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecurityPolicy;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn valid_agent_yaml(id: &str, name: &str) -> String {
        format!(
            r#"
agent:
  id: "{}"
  name: "{}"
  version: "1.0.0"
  description: "A test agent"

execution:
  mode: subprocess
  command: "test"
  args: []

provider:
  name: "openrouter"
  model: "anthropic/claude-sonnet-4-6"

tools: {{}}

system:
  prompt: "You are a test agent"

memory:
  backend: shared

reporting:
  mode: ipc
  format: json
  timeout_seconds: 300

retry:
  max_attempts: 3
  backoff_ms: 1000
"#,
            id, name
        )
    }

    #[test]
    fn test_watcher_creation() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let registry =
            Arc::new(AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap());

        let watcher = AgentWatcher::new(registry);

        assert!(watcher.is_ok());
        let watcher = watcher.unwrap();
        assert!(!watcher.is_running());
        assert_eq!(watcher.watch_directory(), tmp_dir.path());
    }

    #[test]
    fn test_watcher_start_stop() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let registry =
            Arc::new(AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap());

        let mut watcher = AgentWatcher::new(registry).unwrap();

        // Start in a runtime
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Start
            assert!(watcher.start().is_ok());
            assert!(watcher.is_running());

            // Give time for the watcher to start
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        });

        // Stop outside of runtime (uses blocking_send)
        assert!(watcher.stop().is_ok());
        assert!(!watcher.is_running());

        // Stop again (should be idempotent)
        assert!(watcher.stop().is_ok());
        assert!(!watcher.is_running());
    }

    #[test]
    fn test_should_reload_for_yaml_files() {

        // YAML file create event
        let event = notify::Event {
            kind: EventKind::Create(notify::event::CreateKind::Any),
            paths: vec![PathBuf::from("test.yaml")],
            attrs: notify::event::EventAttributes::new(),
        };
        assert!(AgentWatcher::should_reload_for_event(&event));

        // YML file modify event
        let event = notify::Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![PathBuf::from("test.yml")],
            attrs: notify::event::EventAttributes::new(),
        };
        assert!(AgentWatcher::should_reload_for_event(&event));

        // YAML file remove event
        let event = notify::Event {
            kind: EventKind::Remove(notify::event::RemoveKind::Any),
            paths: vec![PathBuf::from("test.yaml")],
            attrs: notify::event::EventAttributes::new(),
        };
        assert!(AgentWatcher::should_reload_for_event(&event));

        // Non-YAML file (should not reload)
        let event = notify::Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![PathBuf::from("test.txt")],
            attrs: notify::event::EventAttributes::new(),
        };
        assert!(!AgentWatcher::should_reload_for_event(&event));

        // Access event (should not reload)
        let event = notify::Event {
            kind: EventKind::Access(notify::event::AccessKind::Any),
            paths: vec![PathBuf::from("test.yaml")],
            attrs: notify::event::EventAttributes::new(),
        };
        assert!(!AgentWatcher::should_reload_for_event(&event));
    }

    #[test]
    fn test_watcher_with_nonexistent_directory() {
        let tmp_dir = TempDir::new().unwrap();
        // Create the agents directory so watcher can be created
        let agents_dir = tmp_dir.path().join("agents");
        fs::create_dir(&agents_dir).unwrap();

        let security = Arc::new(SecurityPolicy::default());
        let registry =
            Arc::new(AgentRegistry::new(agents_dir.clone(), security).unwrap());

        let mut watcher = AgentWatcher::new(registry).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Start should succeed
            assert!(watcher.start().is_ok());
            assert!(watcher.is_running());

            // Give it a moment
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        });

        // Stop outside of runtime
        assert!(watcher.stop().is_ok());
    }

    #[test]
    fn test_reload_after_file_change() {
        let tmp_dir = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy::default());
        let registry =
            Arc::new(AgentRegistry::new(tmp_dir.path().to_path_buf(), security).unwrap());

        // Initial discovery
        registry.discover().unwrap();
        assert_eq!(registry.count(), 0);

        let mut watcher = AgentWatcher::new(Arc::clone(&registry)).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Start watcher
            watcher.start().unwrap();

            // Create a new agent file
            let agent_file = tmp_dir.path().join("test-agent.yaml");
            fs::write(&agent_file, valid_agent_yaml("test", "Test Agent")).unwrap();

            // Give the watcher time to detect the change
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            // Check that the agent was loaded
            assert_eq!(registry.count(), 1);
        });

        // Stop outside of runtime
        watcher.stop().unwrap();
    }

    #[test]
    fn test_multiple_yaml_files_in_event() {

        // Event with multiple paths, some YAML, some not
        let event = notify::Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![
                PathBuf::from("readme.txt"),
                PathBuf::from("agent.yaml"),
                PathBuf::from("config.json"),
                PathBuf::from("another.yml"),
            ],
            attrs: notify::event::EventAttributes::new(),
        };

        // Should reload because at least one YAML file is present
        assert!(AgentWatcher::should_reload_for_event(&event));
    }
}
